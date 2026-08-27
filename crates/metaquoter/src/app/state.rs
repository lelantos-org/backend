use crate::adapters::chain_setup::ChainSetup;
use crate::adapters::univ3::UniV3Quoter;
use crate::adapters::univ4::UniV4Quoter;
use crate::app::config::{ChainCfg, MetaQuoterConfig};
use crate::domain::error::{AppError, AppResult};
use crate::repositories::quoter::Quoter;
use crate::services::quote_service::{QuoteService, RacingQuoteService};
use alloy::primitives::Address;
use alloy::providers::ProviderBuilder;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub quote_service: Arc<dyn QuoteService>,
}

pub async fn build_state(cfg: &MetaQuoterConfig) -> AppResult<AppState> {
    if cfg.chains.is_empty() {
        return Err(AppError::Internal("no chains configured".into()));
    }

    let mut univ3_chains: HashMap<u64, ChainSetup> = HashMap::new();
    let mut univ4_chains: HashMap<u64, ChainSetup> = HashMap::new();

    for c in &cfg.chains {
        let url = c
            .rpc_url
            .parse::<url::Url>()
            .map_err(|e| AppError::Internal(format!("bad rpc url chain {}: {}", c.chain_id, e)))?;

        univ3_chains.insert(
            c.chain_id,
            setup(c, &url, "univ3", c.univ3_quoter, c.univ3_adapter),
        );

        // A chain joins the V4 race only with both addresses configured; one
        // without them stays V3-only rather than erroring.
        if let (Some(quoter), Some(adapter)) = (c.univ4_quoter, c.univ4_adapter) {
            univ4_chains.insert(c.chain_id, setup(c, &url, "univ4", quoter, adapter));
        }
    }

    let mut quoters: Vec<Arc<dyn Quoter>> = vec![Arc::new(UniV3Quoter::new(univ3_chains))];
    // Left out entirely when no chain configures V4, so `supports_chain` is
    // never consulted for a venue that cannot answer.
    if !univ4_chains.is_empty() {
        quoters.push(Arc::new(UniV4Quoter::new(univ4_chains)));
    }

    let quote_service: Arc<dyn QuoteService> = Arc::new(RacingQuoteService::new(
        quoters,
        Duration::from_millis(cfg.race_deadline_ms),
    ));

    Ok(AppState { quote_service })
}

/// One venue's wiring for one chain. Each venue gets its own provider over the
/// same URL, since [`ChainSetup`] owns it and the two maps outlive this loop.
fn setup(
    c: &ChainCfg,
    url: &url::Url,
    venue: &str,
    quoter: Address,
    adapter: Address,
) -> ChainSetup {
    info!(chain_id = c.chain_id, %venue, %quoter, %adapter, "chain wired");
    ChainSetup {
        provider: ProviderBuilder::new().on_http(url.clone()),
        quoter_addr: quoter,
        adapter_addr: adapter,
        masp_fee_bps: c.masp_fee_bps,
    }
}
