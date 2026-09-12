//! Wiring config into the per-chain services.

use crate::adapters::ratelimit::{
    ClientLimiter, ClientQuotas, GlobalLimiter, GlobalQuotas, TrustedHeader,
};
use crate::adapters::upstream::{Endpoint, FALLBACK, HttpUpstream, PRIMARY, Upstream};
use crate::app::config::{ChainCfg, RpcProxyConfig};
use crate::domain::error::{AppError, AppResult};
use crate::domain::targets::Targets;
use crate::services::proxy::{ChainLimits, ChainService};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Clone)]
pub struct AppState {
    pub chains: Arc<HashMap<u64, Arc<ChainService>>>,
    pub max_batch: usize,
    pub trusted_header: Arc<TrustedHeader>,
    /// The per-client buckets, shared by every chain.
    ///
    /// On the state rather than reached for through a chain because the handler
    /// charges admission before it has parsed the body — and therefore before
    /// it knows anything but which chain was addressed.
    pub client_limiter: Arc<ClientLimiter>,
}

pub async fn build_state(cfg: &RpcProxyConfig) -> AppResult<AppState> {
    cfg.validate()?;

    // One instance for the service. Built per chain, a client reading three
    // chains would get three times the budget the single `[rate_limit]` block
    // documents.
    let client_limiter = Arc::new(
        ClientLimiter::new(
            ClientQuotas {
                units_per_second: cfg.rate_limit.client_units_per_second,
                burst_units: cfg.rate_limit.client_burst_units,
                long_units: cfg.rate_limit.client_long_units,
                long_window: Duration::from_secs(cfg.rate_limit.client_long_window_s),
            },
            Default::default(),
        )
        .map_err(AppError::Internal)?,
    );

    // Where the rate-limit key comes from decides whether it means anything: a
    // header that nothing in front sets lets every caller pick its own bucket.
    // Stated once at startup, so a deploy that got it wrong is visible before
    // anyone exploits it.
    match &cfg.trusted_client_ip_header {
        Some(header) => info!(
            %header,
            position = ?cfg.trusted_client_ip_position,
            "client address read from a forwarding header; only safe behind a proxy that sets it"
        ),
        None => info!("client address read from the socket peer"),
    }

    let mut chains = HashMap::new();
    for c in &cfg.chains {
        chains.insert(c.chain_id, Arc::new(wire(c, cfg, client_limiter.clone())?));
    }

    Ok(AppState {
        chains: Arc::new(chains),
        max_batch: cfg.max_batch,
        trusted_header: Arc::new(TrustedHeader {
            name: cfg.trusted_client_ip_header.clone(),
            position: cfg.trusted_client_ip_position,
        }),
        client_limiter,
    })
}

/// One chain's service.
fn wire(
    c: &ChainCfg,
    cfg: &RpcProxyConfig,
    client_limiter: Arc<ClientLimiter>,
) -> AppResult<ChainService> {
    let mut endpoints = vec![Endpoint {
        url: c.upstream_url.clone(),
        archive: c.upstream_archive,
        label: PRIMARY,
    }];
    if let Some(url) = &c.fallback_url {
        endpoints.push(Endpoint {
            url: url.clone(),
            archive: c.fallback_archive,
            label: FALLBACK,
        });
    }
    let upstream: Arc<dyn Upstream> = Arc::new(HttpUpstream::new(
        c.chain_id,
        endpoints,
        cfg.upstream_max_inflight,
    )?);

    let targets = Targets::new(
        c.masp_address,
        c.permit2_address,
        c.erc20_seed.iter().copied(),
        c.venue_seed.iter().copied(),
    );

    // Per chain, unlike the client buckets: this one bounds spend against this
    // chain's own metered upstream.
    let global_limiter = GlobalLimiter::new(
        GlobalQuotas {
            units_per_second: c.upstream_units_per_second,
            burst_units: c.upstream_burst_units,
        },
        Default::default(),
    )
    .map_err(|e| AppError::Internal(format!("chain {}: {e}", c.chain_id)))?;

    // The allowlist size is logged so a misconfigured deploy — 3 contracts
    // where 31 were expected — is visible at startup rather than as failing
    // reads.
    info!(
        chain_id = c.chain_id,
        contracts = targets.len(),
        reorg_depth = c.reorg_depth,
        deploy_block = c.deploy_block.unwrap_or(0),
        archive = c.has_archive(),
        fallback = c.fallback_url.is_some(),
        "chain wired"
    );
    if !c.has_archive() {
        // Not fatal: it degrades the portfolio's earned column to what it shows
        // today, rather than taking the service down.
        warn!(
            chain_id = c.chain_id,
            "no archive endpoint configured; historical reads will be refused \
             and the earned column will stay empty"
        );
    }
    if c.venue_seed.is_empty() {
        warn!(
            chain_id = c.chain_id,
            "no yield venues in the allowlist; if this chain has yielding assets, \
             regenerate venue_seed from the registry's /v1/assets"
        );
    }

    ChainService::new(
        c.chain_id,
        c.masp_address,
        targets,
        ChainLimits {
            max_log_range: cfg.max_log_range,
            max_call_data_bytes: cfg.max_call_data_bytes,
            max_call_gas: cfg.max_call_gas,
            reorg_depth: c.reorg_depth,
            deploy_block: c.deploy_block,
        },
        upstream,
        client_limiter,
        global_limiter,
    )
}
