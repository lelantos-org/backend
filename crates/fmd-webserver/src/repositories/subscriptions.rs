use crate::domain::error::{AppError, AppResult};
use crate::domain::token::TokenHash;
use database::DbPool;
pub use database::models::SubscriptionRow;
use database::schema::subscriptions;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Clone, Insertable)]
#[diesel(table_name = subscriptions)]
struct NewSubscription {
    detection_key: Vec<u8>,
    gamma: i32,
    active: bool,
    token_hash: Vec<u8>,
}

/// Resolve a capability token to the internal subscription id. `None` when it
/// matches nothing; callers must not distinguish that from an empty result set.
pub async fn id_by_token(pool: &DbPool, token: &TokenHash) -> AppResult<Option<i64>> {
    let mut conn = super::conn(pool).await?;
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
    let mut conn = super::conn(pool).await?;
    subscriptions::table
        .filter(subscriptions::token_hash.eq(token.as_bytes()))
        .select(SubscriptionRow::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(|e| AppError::Db(e.to_string()))
}

/// `Ok(None)` when the token is already taken. The unique index on `token_hash`
/// is the authority on token ownership, so a violation is an expected outcome
/// rather than an error and detects a re-registration without a prior lookup.
pub async fn create(
    pool: &DbPool,
    dk: Vec<u8>,
    gamma: i32,
    token: &TokenHash,
) -> AppResult<Option<SubscriptionRow>> {
    use diesel::result::{DatabaseErrorKind, Error as DieselError};

    let mut conn = super::conn(pool).await?;
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
    let mut conn = super::conn(pool).await?;
    diesel::delete(subscriptions::table.filter(subscriptions::token_hash.eq(token.as_bytes())))
        .execute(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
