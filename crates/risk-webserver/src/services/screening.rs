use crate::domain::address::NormalizedAddress;
use crate::domain::error::AppResult;
use crate::domain::responses::{EntryOut, MatchOut, ScreenOut};
use crate::domain::risk::RiskLevel;
use crate::repositories::screened_addresses::{EntryFilter, ScreenedAddressRepo, ScreenedRow};
use moka::future::Cache;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Verdicts are cached per normalized address, negatives included — almost
/// every screened address is clean, so negative caching is where the hit rate
/// comes from.
pub type ScreenCache = Cache<NormalizedAddress, Arc<ScreenOut>>;

const CACHE_CAPACITY: u64 = 10_000;

pub struct ScreeningService {
    repo: Arc<dyn ScreenedAddressRepo>,
    cache: ScreenCache,
}

impl ScreeningService {
    /// `ttl_s` bounds how long the service can lag behind the table. There is
    /// no write path to invalidate on, so a row inserted by SQL becomes
    /// visible only once the entry expires — on every replica independently.
    pub fn new(repo: Arc<dyn ScreenedAddressRepo>, ttl_s: u64) -> Self {
        Self {
            repo,
            cache: shared::cache::build(CACHE_CAPACITY, Duration::from_secs(ttl_s.max(1))),
        }
    }

    /// Screen every address, preserving request order.
    ///
    /// Fail-closed: a DB error propagates rather than being reported as
    /// clean. The service never claims an address is unlisted when it could
    /// not read the list.
    pub async fn screen(&self, addrs: Vec<NormalizedAddress>) -> AppResult<Vec<Arc<ScreenOut>>> {
        let mut out: Vec<Option<Arc<ScreenOut>>> = vec![None; addrs.len()];
        let mut misses: HashMap<&str, Vec<usize>> = HashMap::new();

        for (i, addr) in addrs.iter().enumerate() {
            match self.cache.get(addr).await {
                Some(hit) => out[i] = Some(hit),
                None => misses.entry(addr.chain.as_str()).or_default().push(i),
            }
        }

        for (chain, idxs) in misses {
            let mut wanted: Vec<String> = idxs.iter().map(|&i| addrs[i].address.clone()).collect();
            wanted.sort();
            wanted.dedup();

            let rows = self.repo.find(chain, &wanted).await?;
            let mut by_address: HashMap<&str, Vec<&ScreenedRow>> = HashMap::new();
            for row in &rows {
                by_address
                    .entry(row.address.as_str())
                    .or_default()
                    .push(row);
            }

            let mut built: HashMap<&str, Arc<ScreenOut>> = HashMap::new();
            for address in &wanted {
                let matched = by_address
                    .get(address.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let verdict = Arc::new(build_verdict(chain, address, matched)?);
                self.cache
                    .insert(
                        NormalizedAddress {
                            chain: chain.to_string(),
                            address: address.clone(),
                        },
                        verdict.clone(),
                    )
                    .await;
                built.insert(address.as_str(), verdict);
            }

            for i in idxs {
                out[i] = built.get(addrs[i].address.as_str()).cloned();
            }
        }

        // Every slot was either a cache hit or built above.
        Ok(out.into_iter().flatten().collect())
    }

    pub async fn list_entries(&self, filter: EntryFilter) -> AppResult<Vec<EntryOut>> {
        let rows = self.repo.list(filter).await?;
        rows.into_iter()
            .map(|r| {
                Ok(EntryOut {
                    risk: RiskLevel::from_db(&r.risk)?,
                    chain: r.chain,
                    address: r.address,
                    source: r.source,
                    reason: r.reason,
                    added_at: r.added_at,
                })
            })
            .collect()
    }
}

/// Verdict for one address: highest risk across its listings.
fn build_verdict(chain: &str, address: &str, rows: &[&ScreenedRow]) -> AppResult<ScreenOut> {
    let mut risk = RiskLevel::None;
    let mut matches = Vec::with_capacity(rows.len());
    for row in rows {
        let level = RiskLevel::from_db(&row.risk)?;
        risk = risk.max(level);
        matches.push(MatchOut {
            source: row.source.clone(),
            risk: level,
            reason: row.reason.clone(),
            added_at: row.added_at,
        });
    }
    matches.sort_by(|a, b| b.risk.cmp(&a.risk).then_with(|| a.source.cmp(&b.source)));
    Ok(ScreenOut {
        chain: chain.to_string(),
        address: address.to_string(),
        risk,
        blocked: risk.blocked(),
        matches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::AppError;
    use crate::repositories::screened_addresses::MockScreenedAddressRepo;
    use chrono::{TimeZone, Utc};

    fn addr(address: &str) -> NormalizedAddress {
        NormalizedAddress {
            chain: "evm".to_string(),
            address: address.to_string(),
        }
    }

    fn row(address: &str, risk: &str, source: &str) -> ScreenedRow {
        ScreenedRow {
            chain: "evm".to_string(),
            address: address.to_string(),
            risk: risk.to_string(),
            source: source.to_string(),
            reason: None,
            added_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        }
    }

    /// Repo that answers with `rows` and asserts it is called exactly `times`.
    fn svc_returning(rows: Vec<ScreenedRow>, times: usize) -> ScreeningService {
        let mut repo = MockScreenedAddressRepo::new();
        repo.expect_find().times(times).returning(move |_, _| {
            let rows = rows.clone();
            Box::pin(async move { Ok(rows) })
        });
        ScreeningService::new(Arc::new(repo), 60)
    }

    #[tokio::test]
    async fn test_screen_no_rows_returns_none_not_blocked() {
        let svc = svc_returning(vec![], 1);
        let got = svc.screen(vec![addr("0xaa")]).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].risk, RiskLevel::None);
        assert!(!got[0].blocked);
        assert!(got[0].matches.is_empty());
    }

    #[tokio::test]
    async fn test_screen_multiple_sources_returns_max_risk() {
        let svc = svc_returning(
            vec![row("0xaa", "low", "manual"), row("0xaa", "banned", "ofac")],
            1,
        );
        let got = svc.screen(vec![addr("0xaa")]).await.unwrap();
        assert_eq!(got[0].risk, RiskLevel::Banned);
        assert_eq!(got[0].matches.len(), 2);
        // Highest risk first.
        assert_eq!(got[0].matches[0].source, "ofac");
    }

    #[tokio::test]
    async fn test_screen_banned_is_blocked() {
        let svc = svc_returning(vec![row("0xaa", "banned", "ofac")], 1);
        let got = svc.screen(vec![addr("0xaa")]).await.unwrap();
        assert!(got[0].blocked);
    }

    #[tokio::test]
    async fn test_screen_medium_is_not_blocked() {
        let svc = svc_returning(vec![row("0xaa", "medium", "manual")], 1);
        let got = svc.screen(vec![addr("0xaa")]).await.unwrap();
        assert_eq!(got[0].risk, RiskLevel::Medium);
        assert!(!got[0].blocked);
    }

    #[tokio::test]
    async fn test_screen_second_call_hits_cache_and_skips_repo() {
        let svc = svc_returning(vec![row("0xaa", "banned", "ofac")], 1);
        let first = svc.screen(vec![addr("0xaa")]).await.unwrap();
        let second = svc.screen(vec![addr("0xaa")]).await.unwrap();
        assert_eq!(first[0].risk, second[0].risk);
    }

    #[tokio::test]
    async fn test_screen_caches_negatives_too() {
        let svc = svc_returning(vec![], 1);
        svc.screen(vec![addr("0xaa")]).await.unwrap();
        let again = svc.screen(vec![addr("0xaa")]).await.unwrap();
        assert_eq!(again[0].risk, RiskLevel::None);
    }

    #[tokio::test]
    async fn test_screen_batch_issues_one_repo_call() {
        let svc = svc_returning(vec![row("0xbb", "high", "ofac")], 1);
        let got = svc
            .screen(vec![addr("0xaa"), addr("0xbb"), addr("0xcc")])
            .await
            .unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].risk, RiskLevel::None);
        assert_eq!(got[1].risk, RiskLevel::High);
        assert!(got[1].blocked);
        assert_eq!(got[2].risk, RiskLevel::None);
    }

    #[tokio::test]
    async fn test_screen_preserves_request_order_with_duplicates() {
        let svc = svc_returning(vec![row("0xbb", "banned", "ofac")], 1);
        let got = svc
            .screen(vec![addr("0xbb"), addr("0xaa"), addr("0xbb")])
            .await
            .unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].address, "0xbb");
        assert_eq!(got[1].address, "0xaa");
        assert_eq!(got[2].address, "0xbb");
        assert!(got[0].blocked);
        assert!(!got[1].blocked);
        assert!(got[2].blocked);
    }

    #[tokio::test]
    async fn test_screen_db_error_propagates_not_clean() {
        let mut repo = MockScreenedAddressRepo::new();
        repo.expect_find()
            .returning(|_, _| Box::pin(async { Err(AppError::Db("connection reset".into())) }));
        let svc = ScreeningService::new(Arc::new(repo), 60);
        let err = svc.screen(vec![addr("0xaa")]).await.unwrap_err();
        assert!(matches!(err, AppError::Db(_)));
    }

    #[tokio::test]
    async fn test_screen_unknown_stored_risk_is_internal_error() {
        let svc = svc_returning(vec![row("0xaa", "severe", "ofac")], 1);
        let err = svc.screen(vec![addr("0xaa")]).await.unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }
}
