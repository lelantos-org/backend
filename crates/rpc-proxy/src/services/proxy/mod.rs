//! Serving one chain: limit, allowlist, cache, forward.
//!
//! Split in three. This module holds the service and the shape of one request:
//! the rate-limit charge, the head warm-up, the allowlist it validates against,
//! and what each outcome is counted as. [`plan`] decides how each call is
//! served and puts the answers back together. [`fetch`] gets the values,
//! through the cache, the in-flight map and the upstream round trip.
//!
//! # Coalescing and batching
//!
//! Coalescing collapses concurrent callers of one key onto a single upstream
//! call. Batching collapses several keys from one client into a single round
//! trip. Both apply to every call: a request claims all of its misses in the
//! [`InFlight`] map, forwards the ones nobody else is fetching as one batch, and
//! waits on the rest.
//!
//! That matters because the herds arrive batched. viem batches whatever a
//! wallet issues inside its wait window, so the once-a-second `eth_blockNumber`
//! poll usually rides alongside that wallet's own reads — and a coalescing path
//! reserved for lone calls would let every such batch re-fetch the head.
//!
//! The round trip itself packs `eth_call`s that share a block into one
//! Multicall3 call where the chain has it; see
//! [`multicall`](crate::domain::multicall).

mod fetch;
mod plan;

use crate::adapters::ratelimit::{ClientKey, ClientLimiter, Decision, GlobalLimiter};
use crate::adapters::upstream::Upstream;
use crate::app::cache::Caches;
use crate::app::inflight::InFlight;
use crate::app::log_throttle::LogThrottle;
use crate::domain::allowlist::{Method, Policy as Allow};
use crate::domain::cache_key::{CacheKey, key as cache_key};
use crate::domain::error::{AppError, AppResult};
use crate::domain::jsonrpc::{CachedResult, Reply, Request, Response};
use crate::domain::policy::Class;
use crate::domain::targets::{self, Rejection, Targets};
use crate::domain::tip::Tip;
use alloy::primitives::Address;
use fetch::{Pending, Shared};
use plan::Plan;
use serde_json::value::RawValue;
use shared::metrics::name;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OnceCell;
use tracing::{debug, warn};

/// Per-chain limits, lifted out of config so the service does not hold the
/// whole config tree.
///
/// Named to distinguish it from `shared::router::Limits`, which bounds one HTTP
/// request rather than one chain's reads.
#[derive(Debug, Clone, Copy)]
pub struct ChainLimits {
    pub max_log_range: u64,
    pub max_call_data_bytes: usize,
    pub max_call_gas: u64,
    pub reorg_depth: u64,
    /// See [`crate::domain::allowlist::Policy::deploy_block`].
    pub deploy_block: Option<u64>,
}

/// Everything needed to serve one chain.
pub struct ChainService {
    pub chain_id: u64,
    /// Pre-serialized `eth_chainId` answer. Immutable, so it is built once and
    /// never involves the upstream.
    chain_id_result: CachedResult,
    masp: Address,
    targets: Targets,
    limits: ChainLimits,
    upstream: Arc<dyn Upstream>,
    caches: Caches,
    /// Misses currently being fetched.
    inflight: InFlight<CacheKey, Shared>,
    /// Whether Multicall3 is deployed here. Unset until probed; see
    /// [`Self::has_multicall`].
    multicall: OnceCell<bool>,
    /// Repeats of the WARNs a caller or a changed chain can trigger per request.
    notices: LogThrottle<Notice>,
    tip: Tip,
    /// Shared with every other chain: one client, one budget. See
    /// [`ClientLimiter`].
    client_limiter: Arc<ClientLimiter>,
    /// This chain's own credit guard.
    global_limiter: GlobalLimiter,
}

impl ChainService {
    pub fn new(
        chain_id: u64,
        masp: Address,
        targets: Targets,
        limits: ChainLimits,
        upstream: Arc<dyn Upstream>,
        client_limiter: Arc<ClientLimiter>,
        global_limiter: GlobalLimiter,
    ) -> AppResult<Self> {
        let chain_id_result = RawValue::from_string(format!("\"0x{chain_id:x}\""))
            .map_err(|e| AppError::Internal(format!("encode chain id: {e}")))?
            .into();
        Ok(Self {
            chain_id,
            chain_id_result,
            masp,
            targets,
            limits,
            upstream,
            caches: Caches::new(),
            inflight: InFlight::new(),
            multicall: OnceCell::new(),
            notices: LogThrottle::new(NOTICE_WINDOW, NOTICE_CAPACITY),
            tip: Tip::new(),
            client_limiter,
            global_limiter,
        })
    }

    pub fn caches(&self) -> &Caches {
        &self.caches
    }

    /// Free slots in this chain's upstream concurrency cap, for the gauge.
    pub fn upstream_permits_available(&self) -> usize {
        self.upstream.permits_available()
    }

    /// Serve a batch of requests, in order.
    ///
    /// The rate limit is charged for the whole batch before anything else
    /// happens, so a refusal is a single 429 rather than a partly-served batch
    /// the client has to reconcile.
    pub async fn serve(&self, reqs: &[Request], client: ClientKey) -> AppResult<Vec<Response>> {
        self.charge(reqs, client)?;

        // `eth_getLogs` with an open `toBlock` cannot be range-checked without a
        // head, and the allowlist refuses it rather than guess. Resolving the
        // head here keeps that refusal to a genuine cold start: the answer is
        // cached for a second anyway, so this costs at most one call.
        if self.tip.get().is_none()
            && reqs
                .iter()
                .any(|r| Method::parse(&r.method) == Some(Method::EthGetLogs))
        {
            self.warm_tip().await;
        }

        let plans: Vec<Plan> = reqs.iter().map(|r| self.plan(r)).collect();
        let fetched = self.fetch(reqs, &plans).await?;
        Ok(self.assemble(reqs, plans, fetched))
    }

    /// Charge every bucket for the whole batch's weight.
    fn charge(&self, reqs: &[Request], client: ClientKey) -> AppResult<()> {
        let units: u32 = reqs
            .iter()
            .map(|r| {
                Method::parse(&r.method)
                    // An unknown method still costs something to parse and
                    // refuse, so it is charged the cheapest real weight rather
                    // than nothing — otherwise a flood of them is free.
                    .map_or(1, |m| m.weight(r.params()))
            })
            .sum();

        // Per-client before chain-wide, so a single abusive client is
        // attributed to itself rather than showing up as the whole chain being
        // saturated.
        let decision = match self.client_limiter.check(client, units) {
            Decision::Allow => self.global_limiter.check(units),
            deny => deny,
        };

        decision.into_result().inspect_err(|e| {
            // DEBUG: a refusal is the limiter working, and a client being
            // refused must not be able to fill the log by trying again.
            debug!(units, error = %e, "rate limited");
            for r in reqs {
                match Method::parse(&r.method) {
                    Some(m) => self.count(m, "rate_limited"),
                    None => self.count_unknown_method(),
                }
            }
        })
    }

    /// Pull the head in so a cold-start `eth_getLogs` is not refused for want of
    /// one. A failure here is not fatal: the allowlist will refuse the open
    /// range with a message telling the client to retry.
    ///
    /// Goes through the head cache rather than straight to the upstream, so a
    /// burst arriving while the tip is unknown produces one call rather than
    /// one per request. That window is a cold start in the normal case, but it
    /// is permanent if an upstream keeps answering with a block number `Tip`
    /// cannot parse — and an uncoalesced call on that path would be an extra
    /// paid request for every `eth_getLogs` this service ever served.
    ///
    /// It is the same key a client's own `eth_blockNumber` uses, so the warm-up
    /// also serves the next caller to ask.
    async fn warm_tip(&self) {
        let head = Pending {
            method: Method::EthBlockNumber,
            key: cache_key(self.chain_id, Method::EthBlockNumber, &[]),
            class: Some(Class::Head),
            params: None,
        };

        // `resolve` checks the cache before forwarding, so a head cached but
        // unparseable by `Tip` is re-read here rather than re-fetched.
        let mut out = HashMap::new();
        if self.resolve(vec![&head], &mut out).await.is_ok()
            && let Some(Reply::Result(raw)) = out.get(&head.key)
        {
            self.tip.observe_block_number(raw);
        }
    }

    /// This chain's allowlist, at the current head.
    fn allowlist(&self) -> Allow<'_> {
        Allow {
            masp: self.masp,
            targets: &self.targets,
            max_log_range: self.limits.max_log_range,
            max_call_data_bytes: self.limits.max_call_data_bytes,
            max_call_gas: self.limits.max_call_gas,
            tip: self.tip.get(),
            deploy_block: self.limits.deploy_block,
        }
    }

    /// Record how one request was served.
    ///
    /// Outcomes are mutually exclusive and every request gets exactly one, so
    /// these sum to the number of requests received — counting a served
    /// multicall as the calls inside it, since those are what hit or miss the
    /// cache. A refused multicall counts once. Upstream failures are not
    /// among them: a call that reached the upstream counts as a `miss` here
    /// whatever came back, and how it fared is
    /// `rpc_proxy_upstream_calls_total`.
    fn count(&self, method: Method, outcome: &'static str) {
        metrics::counter!(
            name::RPC_PROXY_REQUESTS,
            "chain" => self.chain_id.to_string(),
            // Always an allowlisted `&'static str`, never the caller's own
            // string, which would let a scanner mint unbounded time series.
            "method" => method.label(),
            "outcome" => outcome,
        )
        .increment(1);
    }

    /// Record a request naming a method this endpoint does not serve.
    ///
    /// Separate from [`Self::count`] because there is no [`Method`] to label it
    /// with, and the caller's string must never become one.
    fn count_unknown_method(&self) {
        metrics::counter!(
            name::RPC_PROXY_REQUESTS,
            "chain" => self.chain_id.to_string(),
            "method" => UNKNOWN_METHOD,
            "outcome" => "rejected_method",
        )
        .increment(1);
    }

    /// Count and log an `eth_call` refused for its target.
    ///
    /// With no runtime refresh of the allowlist, this is the only signal that a
    /// token was registered on-chain without an `rpc-proxy` converge — so a
    /// refused contract or function is logged at WARN with the address, not left
    /// to a metric alone. Once per target per window: a caller repeating the
    /// call must not be able to repeat the line. A malformed call says nothing
    /// about the allowlist and stays at DEBUG.
    fn count_target_rejection(&self, method: Method, req: &Request) {
        if method != Method::EthCall {
            return;
        }
        let (to, selector) = targets::call_target(req.params());
        if let Err(rejection) = self.targets.check(to, selector) {
            metrics::counter!(
                name::RPC_PROXY_REJECTED_TARGET,
                "chain" => self.chain_id.to_string(),
                "class" => rejection.class_label(),
            )
            .increment(1);
            if let Rejection::Malformed(_) = rejection {
                debug!(detail = %rejection.message(), "eth_call refused as malformed");
            } else if self.notices.admit(Notice::Target(rejection.clone())) {
                warn!(
                    chain_id = self.chain_id,
                    class = rejection.class_label(),
                    detail = %rejection.message(),
                    "eth_call target refused; if this names a real contract, the allowlist needs a converge \
                     (repeats suppressed for 10m)"
                );
            }
        }
    }
}

/// A WARN that repeats per request, keyed by what it is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Notice {
    /// An `eth_call` refused for its contract or function.
    Target(Rejection),
    /// A Multicall3 pack whose answer did not decode.
    UnreadablePack,
}

/// How long one notice is suppressed after it is written. Long enough that a
/// flood is a handful of lines an hour; short enough that a problem still
/// present is still visible.
const NOTICE_WINDOW: Duration = Duration::from_secs(600);

/// Distinct notices remembered per chain. A caller can mint a new refused
/// address per call, so this is bounded; see [`LogThrottle::admit`].
const NOTICE_CAPACITY: usize = 1024;

/// Method label for a request naming something this endpoint does not serve.
const UNKNOWN_METHOD: &str = "<unknown>";

/// Generic server-side error code, for the case where a batch slot came back
/// with neither a value nor a verdict.
const SERVER_ERROR: i64 = -32000;
