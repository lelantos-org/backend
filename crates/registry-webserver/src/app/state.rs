use crate::app::cache::AppCache;
use crate::app::config::RegistryConfig;
use crate::domain::responses::ChainsResponse;
use crate::services;
use anyhow::{Context, Result};
use database::DbPool;
use prices::{DefiLlama, PriceService};
use std::sync::Arc;
use std::time::Duration;

/// Everything a handler may reach.
///
/// Holds no chain client: the deployment registry is resolved at boot and the
/// catalog comes from the database. The one component that reads a chain — the
/// venue-APY worker — owns its own endpoint and never serves a request.
#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub cfg: Arc<RegistryConfig>,
    pub cache: AppCache,
    /// The published registry, resolved and validated once at boot.
    ///
    /// Behind an `Arc` because it is immutable after startup and every request
    /// serves the same body; a handler clones the handle, not the chains.
    pub chains: Arc<ChainsResponse>,
    /// Spot prices for `/v1/prices`, provider cache included: a token the
    /// providers cannot price is asked about once per TTL rather than once per
    /// request.
    pub prices: Arc<PriceService>,
}

impl AppState {
    /// Whether this deployment declares `chain_id`.
    ///
    /// The registry defines which chains exist, so a query naming any other is a
    /// 404 rather than an empty answer.
    pub fn serves_chain(&self, chain_id: i64) -> bool {
        self.cfg.chains.iter().any(|c| c.chain_id == chain_id)
    }

    /// Every chain this deployment serves.
    ///
    /// The all-chains form of a catalog read: one query for the lot rather than
    /// a pooled connection per chain.
    pub fn chain_ids(&self) -> Vec<i64> {
        self.cfg.chains.iter().map(|c| c.chain_id).collect()
    }
}

/// Fallible because resolving the registry parses the operator's addresses, and
/// because the price client parses its base URL. A malformed one stops the
/// service starting rather than reaching a wallet.
pub fn build_state(cfg: Arc<RegistryConfig>, pool: DbPool) -> Result<AppState> {
    let chains = Arc::new(services::chains::build(&cfg.chains)?);
    let cache = AppCache::new(cfg.cache_ttl_s);
    let prices = Arc::new(PriceService::new(
        // One provider today; a second one is another entry in this vector.
        vec![Arc::new(
            DefiLlama::new(
                &cfg.token_prices.base_url,
                Duration::from_millis(cfg.token_prices.timeout_ms),
            )
            .context("build price client")?,
        )],
        Duration::from_secs(cfg.token_prices.ttl_s),
    ));
    Ok(AppState {
        pool,
        cfg,
        cache,
        chains,
        prices,
    })
}
