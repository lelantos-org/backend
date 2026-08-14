use crate::domain::error::{AppError, AppResult};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use database::DbPool;
use database::schema::screened_addresses;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = screened_addresses)]
pub struct ScreenedRow {
    pub chain: String,
    pub address: String,
    pub risk: String,
    pub source: String,
    pub reason: Option<String>,
    pub added_at: DateTime<Utc>,
}

/// Filters for the audit listing.
#[derive(Debug, Clone)]
pub struct EntryFilter {
    pub chain: Option<String>,
    pub source: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

/// Read-only access to `screened_addresses`.
///
/// The service holds this as `Arc<dyn ScreenedAddressRepo>` so screening
/// policy can be tested without Postgres. There is deliberately no write
/// method: the list is populated out-of-band by SQL.
#[async_trait]
#[cfg_attr(test, mockall::automock)]
pub trait ScreenedAddressRepo: Send + Sync {
    /// Every row listing any of `addrs` within `chain`. One query regardless
    /// of how many addresses are asked for.
    async fn find(&self, chain: &str, addrs: &[String]) -> AppResult<Vec<ScreenedRow>>;

    async fn list(&self, filter: EntryFilter) -> AppResult<Vec<ScreenedRow>>;
}

pub struct PgScreenedAddressRepo {
    pool: DbPool,
}

impl PgScreenedAddressRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ScreenedAddressRepo for PgScreenedAddressRepo {
    async fn find(&self, chain: &str, addrs: &[String]) -> AppResult<Vec<ScreenedRow>> {
        if addrs.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;
        screened_addresses::table
            .filter(screened_addresses::chain.eq(chain))
            .filter(screened_addresses::address.eq_any(addrs))
            .select(ScreenedRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| AppError::Db(e.to_string()))
    }

    async fn list(&self, filter: EntryFilter) -> AppResult<Vec<ScreenedRow>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;
        let mut q = screened_addresses::table.into_boxed();
        if let Some(c) = filter.chain {
            q = q.filter(screened_addresses::chain.eq(c));
        }
        if let Some(s) = filter.source {
            q = q.filter(screened_addresses::source.eq(s));
        }
        q.order((
            screened_addresses::chain.asc(),
            screened_addresses::address.asc(),
            screened_addresses::source.asc(),
        ))
        .limit(filter.limit)
        .offset(filter.offset)
        .select(ScreenedRow::as_select())
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
    }
}
