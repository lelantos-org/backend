//! Process-wide state: the racing quote service, and the per-chain provider
//! and venue wiring it is built from.

use crate::adapters::chain_setup::ChainSetup;
use crate::adapters::univ3::UniV3Quoter;
use crate::adapters::univ4::UniV4Quoter;
use crate::app::config::{ChainCfg, MetaQuoterConfig};
use crate::domain::error::{AppError, AppResult};
use crate::repositories::quoter::Quoter;
use crate::services::quote_service::{QuoteService, RacingQuoteService};
use alloy::primitives::Address;
use alloy::providers::{ProviderBuilder, RootProvider};
use chain_types::rpc::{HttpTransport, RpcEndpoint, RpcTimeouts};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub quote_service: Arc<dyn QuoteService>,
}

/// Deadline for one quote call.
///
/// A quote is a single `eth_call` against a venue quoter, and the router races
/// several and answers on a request deadline of its own, so this only has to
/// stop a hung node from holding a request open longer than the router would
/// wait. Alloy's default client has no timeout at all.
const TIMEOUTS: RpcTimeouts = RpcTimeouts::request(10);

pub async fn build_state(cfg: &MetaQuoterConfig) -> AppResult<AppState> {
    cfg.validate()?;

    let (univ3_chains, univ4_chains) = wire_chains(cfg)?;

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

/// The per-venue chain maps the quoters are built from, split out so the wiring
/// rules can be tested without standing up an `AppState`.
fn wire_chains(
    cfg: &MetaQuoterConfig,
) -> AppResult<(HashMap<u64, ChainSetup>, HashMap<u64, ChainSetup>)> {
    let mut univ3_chains = HashMap::new();
    let mut univ4_chains = HashMap::new();

    for c in &cfg.chains {
        let rpc = RpcEndpoint::new(&c.rpc_url, TIMEOUTS)
            .map_err(|e| AppError::Internal(format!("bad rpc url chain {}: {}", c.chain_id, e)))?;

        // One provider per chain, shared by every venue on it. `RootProvider` is
        // a handle over a reference-counted transport, so the clone below reuses
        // the connection pool; a second provider built from the same URL would
        // open a second pool against the same node for no gain.
        let provider = ProviderBuilder::new().on_client(rpc.client());

        univ3_chains.insert(
            c.chain_id,
            setup(
                c,
                provider.clone(),
                "univ3",
                c.univ3_quoter,
                c.univ3_adapter,
            ),
        );

        // A chain joins the V4 race only with both addresses configured; one
        // without them stays V3-only rather than erroring.
        if let (Some(quoter), Some(adapter)) = (c.univ4_quoter, c.univ4_adapter) {
            univ4_chains.insert(c.chain_id, setup(c, provider, "univ4", quoter, adapter));
        }
    }

    Ok((univ3_chains, univ4_chains))
}

/// One venue's wiring for one chain.
fn setup(
    c: &ChainCfg,
    provider: RootProvider<HttpTransport>,
    venue: &str,
    quoter: Address,
    adapter: Address,
) -> ChainSetup {
    info!(chain_id = c.chain_id, %venue, %quoter, %adapter, "chain wired");
    ChainSetup {
        provider,
        quoter_addr: quoter,
        adapter_addr: adapter,
        masp_fee_bps: c.masp_fee_bps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    const ADDR: Address = address!("1111111111111111111111111111111111111111");

    fn chain(chain_id: u64, v4: bool) -> ChainCfg {
        ChainCfg {
            chain_id,
            rpc_url: "http://node:8545".into(),
            univ3_quoter: ADDR,
            univ3_adapter: ADDR,
            univ4_quoter: v4.then_some(ADDR),
            univ4_adapter: v4.then_some(ADDR),
            masp_fee_bps: 0,
        }
    }

    fn cfg(chains: Vec<ChainCfg>) -> MetaQuoterConfig {
        MetaQuoterConfig {
            listen_addr: "0.0.0.0:8081".into(),
            chains,
            race_deadline_ms: 1_500,
        }
    }

    /// Every configured chain is quoted on V3; only a chain carrying both V4
    /// addresses joins the V4 map.
    #[test]
    fn v4_is_wired_only_for_chains_that_configure_it() {
        let (v3, v4) = wire_chains(&cfg(vec![chain(1, true), chain(8453, false)])).unwrap();

        assert_eq!(v3.len(), 2);
        assert_eq!(v4.keys().copied().collect::<Vec<_>>(), vec![1]);
    }

    /// Half a V4 config is not a V4 config: quoting against a lens with no
    /// adapter to bind the route to would hand back an unexecutable quote.
    #[test]
    fn a_half_configured_v4_chain_stays_v3_only() {
        let mut c = chain(1, true);
        c.univ4_adapter = None;

        let (v3, v4) = wire_chains(&cfg(vec![c])).unwrap();
        assert_eq!(v3.len(), 1);
        assert!(v4.is_empty());
    }

    #[test]
    fn a_bad_rpc_url_is_rejected() {
        let mut c = chain(1, false);
        c.rpc_url = "not a url".into();
        assert!(wire_chains(&cfg(vec![c])).is_err());
    }

    /// `build_state` runs `validate` first, so a config the validator refuses
    /// cannot reach the wiring.
    #[tokio::test]
    async fn build_state_rejects_an_invalid_config() {
        assert!(build_state(&cfg(vec![])).await.is_err());
        assert!(
            build_state(&cfg(vec![chain(1, false), chain(1, false)]))
                .await
                .is_err()
        );
    }
}
