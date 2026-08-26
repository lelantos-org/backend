//! Price oracle abstraction and its Coinbase HTTP implementation.
//!
//! `PriceOracle` returns the price of one unit of `base` denominated in `quote`,
//! so `price("ETH", "USDC")` is roughly 3000.0. The fee estimator uses it to
//! translate gas cost in native wei into accepted fee-token amounts.
//!
//! The Coinbase implementation adds:
//!   - a TTL cache per `(base, quote)` pair
//!   - single-flight per key, so N concurrent estimators cause one HTTP request
//!   - a stale-cache fallback within `max_stale` when a fetch fails
//!   - an optional USD-cross fallback when the direct pair 404s

use crate::app::config::PriceOracleCfg;
use crate::domain::error::{AppError, AppResult};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::warn;

#[async_trait]
pub trait PriceOracle: Send + Sync {
    async fn price(&self, base: &str, quote: &str) -> AppResult<f64>;
}

#[derive(Debug, Clone, Copy)]
pub enum PriceEndpoint {
    Spot,
    Buy,
}

impl PriceEndpoint {
    fn path(&self) -> &'static str {
        match self {
            PriceEndpoint::Spot => "spot",
            PriceEndpoint::Buy => "buy",
        }
    }
}

#[derive(Clone, Copy)]
struct CachedPrice {
    price: f64,
    fetched_at: Instant,
}

pub struct CoinbaseOracle {
    http: reqwest::Client,
    base_url: String,
    endpoint: PriceEndpoint,
    ttl: Duration,
    max_stale: Duration,
    allow_usd_cross: bool,
    cache: RwLock<HashMap<(String, String), CachedPrice>>,
    inflight: Mutex<InflightMap>,
}

type InflightMap = HashMap<(String, String), broadcast::Sender<Result<f64, String>>>;

#[derive(Deserialize)]
struct CoinbaseResp {
    data: CoinbaseData,
}

#[derive(Deserialize)]
struct CoinbaseData {
    amount: String,
}

impl CoinbaseOracle {
    pub fn new(cfg: &PriceOracleCfg) -> AppResult<Self> {
        let endpoint = match cfg.endpoint.as_str() {
            "spot" => PriceEndpoint::Spot,
            "buy" => PriceEndpoint::Buy,
            other => {
                return Err(AppError::Internal(format!(
                    "price_oracle.endpoint must be 'spot' or 'buy', got '{other}'"
                )));
            }
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| AppError::Internal(format!("reqwest client: {e}")))?;
        Ok(Self {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            endpoint,
            ttl: Duration::from_secs(cfg.cache_ttl_s),
            max_stale: Duration::from_secs(cfg.max_stale_s),
            allow_usd_cross: cfg.allow_usd_cross,
            cache: RwLock::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
        })
    }

    fn cache_lookup(
        &self,
        cache: &HashMap<(String, String), CachedPrice>,
        base: &str,
        quote: &str,
    ) -> Option<CachedPrice> {
        cache.get(&(base.to_string(), quote.to_string())).copied()
    }

    async fn fetch_direct(&self, base: &str, quote: &str) -> AppResult<f64> {
        let url = format!(
            "{}/prices/{}-{}/{}",
            self.base_url,
            base,
            quote,
            self.endpoint.path()
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::Oracle(format!("GET {url}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AppError::Oracle(format!("GET {url}: status {status}")));
        }
        let body: CoinbaseResp = resp
            .json()
            .await
            .map_err(|e| AppError::Oracle(format!("decode {url}: {e}")))?;
        body.data
            .amount
            .parse::<f64>()
            .map_err(|e| AppError::Oracle(format!("parse amount '{}': {e}", body.data.amount)))
    }

    /// Single-flight wrapper: one fetch in flight per `(base, quote)`, with other
    /// callers on the same key awaiting its result.
    async fn single_flight_fetch(&self, base: &str, quote: &str) -> AppResult<f64> {
        let key = (base.to_string(), quote.to_string());

        let rx_opt = {
            let inflight = self.inflight.lock().await;
            inflight.get(&key).map(|tx| tx.subscribe())
        };
        if let Some(mut rx) = rx_opt {
            return match rx.recv().await {
                Ok(Ok(p)) => Ok(p),
                Ok(Err(e)) => Err(AppError::Oracle(e)),
                Err(e) => Err(AppError::Oracle(format!("inflight broadcast: {e}"))),
            };
        }

        let (tx, _) = broadcast::channel::<Result<f64, String>>(8);
        {
            let mut inflight = self.inflight.lock().await;
            // Re-check: another task may have inserted while this one waited.
            if let Some(existing) = inflight.get(&key) {
                let mut rx = existing.subscribe();
                drop(inflight);
                return match rx.recv().await {
                    Ok(Ok(p)) => Ok(p),
                    Ok(Err(e)) => Err(AppError::Oracle(e)),
                    Err(e) => Err(AppError::Oracle(format!("inflight broadcast: {e}"))),
                };
            }
            inflight.insert(key.clone(), tx.clone());
        }

        let result = self.do_fetch_with_cross(base, quote).await;

        {
            let mut inflight = self.inflight.lock().await;
            inflight.remove(&key);
        }
        let broadcast_value = match &result {
            Ok(p) => Ok(*p),
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(broadcast_value);
        result
    }

    async fn do_fetch_with_cross(&self, base: &str, quote: &str) -> AppResult<f64> {
        match self.fetch_direct(base, quote).await {
            Ok(p) => Ok(p),
            Err(direct_err) if self.allow_usd_cross && base != "USD" && quote != "USD" => {
                let base_usd = self.fetch_direct(base, "USD").await.map_err(|e| {
                    AppError::Oracle(format!(
                        "direct {base}-{quote} failed ({direct_err}); USD cross base failed: {e}"
                    ))
                })?;
                let quote_usd = self.fetch_direct(quote, "USD").await.map_err(|e| {
                    AppError::Oracle(format!(
                        "direct {base}-{quote} failed ({direct_err}); USD cross quote failed: {e}"
                    ))
                })?;
                if quote_usd <= 0.0 {
                    return Err(AppError::Oracle(format!(
                        "USD cross: {quote}-USD non-positive: {quote_usd}"
                    )));
                }
                self.store(base, "USD", base_usd).await;
                self.store(quote, "USD", quote_usd).await;
                Ok(base_usd / quote_usd)
            }
            Err(e) => Err(e),
        }
    }

    async fn store(&self, base: &str, quote: &str, price: f64) {
        let mut cache = self.cache.write().await;
        cache.insert(
            (base.to_string(), quote.to_string()),
            CachedPrice {
                price,
                fetched_at: Instant::now(),
            },
        );
    }
}

#[async_trait]
impl PriceOracle for CoinbaseOracle {
    async fn price(&self, base: &str, quote: &str) -> AppResult<f64> {
        {
            let cache = self.cache.read().await;
            if let Some(c) = self.cache_lookup(&cache, base, quote)
                && c.fetched_at.elapsed() < self.ttl
            {
                return Ok(c.price);
            }
        }

        match self.single_flight_fetch(base, quote).await {
            Ok(p) => {
                self.store(base, quote, p).await;
                Ok(p)
            }
            Err(e) => {
                // `max_stale` extends past the TTL rather than being measured from
                // the same instant. This branch is reachable only once the entry is
                // older than `ttl`, so a `max_stale` measured from `fetched_at`
                // would make the fallback unreachable whenever
                // `max_stale <= ttl`, which the defaults are.
                let cache = self.cache.read().await;
                if let Some(c) = self.cache_lookup(&cache, base, quote)
                    && c.fetched_at.elapsed() < self.ttl + self.max_stale
                {
                    warn!(
                        base,
                        quote,
                        age_s = c.fetched_at.elapsed().as_secs(),
                        error = %e,
                        "oracle fetch failed; serving stale cached price"
                    );
                    return Ok(c.price);
                }
                Err(e)
            }
        }
    }
}
