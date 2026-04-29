use crate::domain::error::{AppError, AppResult};
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::{matches, notes};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Queryable)]
pub struct MatchedNote {
    pub note_id: i64,
    pub chain_id: i64,
    pub cm: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub block_number: i64,
    pub leaf_index: i64,
    pub eph_pub_x: BigDecimal,
    pub eph_pub_y: BigDecimal,
}

pub async fn list_for_subscription(
    pool: &DbPool,
    subscription_id: i64,
    after_note_id: i64,
    limit: i64,
) -> AppResult<Vec<MatchedNote>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    matches::table
        .inner_join(notes::table.on(notes::id.eq(matches::note_id)))
        .filter(matches::subscription_id.eq(subscription_id))
        .filter(matches::note_id.gt(after_note_id))
        .order(matches::note_id.asc())
        .limit(limit)
        .select((
            matches::note_id,
            matches::chain_id,
            notes::cm,
            notes::ciphertext,
            notes::block_number,
            notes::leaf_index,
            notes::eph_pub_x,
            notes::eph_pub_y,
        ))
        .load::<MatchedNote>(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
