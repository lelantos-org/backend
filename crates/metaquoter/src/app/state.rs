use crate::adapters::univ3::UniV3Quoter;
use crate::adapters::univ3::quoter::ChainSetup;
use crate::app::config::MetaQuoterConfig;
use crate::domain::error::{AppError, AppResult};
use crate::repositories::quoter::Quoter;
use crate::services::quote_service::{QuoteService, RacingQuoteService};
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
    for c in &cfg.chains {
        let url = c
            .rpc_url
            .parse::<url::Url>()
            .map_err(|e| AppError::Internal(format!("bad rpc url chain {}: {}", c.chain_id, e)))?;
        let provider = ProviderBuilder::new().on_http(url);
        univ3_chains.insert(
            c.chain_id,
            ChainSetup {
                provider,
                quoter_addr: c.univ3_quoter,
                adapter_addr: c.univ3_adapter,
                masp_fee_bps: c.masp_fee_bps,
            },
        );
        info!(
            chain_id = c.chain_id,
            quoter = %c.univ3_quoter,
            adapter = %c.univ3_adapter,
            "univ3 chain wired"
        );
    }

    let univ3: Arc<dyn Quoter> = Arc::new(UniV3Quoter::new(univ3_chains));
    let quote_service: Arc<dyn QuoteService> = Arc::new(RacingQuoteService::new(
        vec![univ3],
        Duration::from_millis(cfg.race_deadline_ms),
    ));

    Ok(AppState { quote_service })
}
