//! Sequential failover across one chain's endpoints, inside one deadline.

use super::body::Body;
use super::{Answer, Endpoint, OutboundCall, Upstream};
use crate::domain::error::{AppError, AppResult};
use async_trait::async_trait;
use std::time::Duration;
use tracing::{debug, warn};

/// Deadline for one upstream attempt.
///
/// Below the registry's 30s, which is tuned for archive-wide APY reads. An
/// `eth_getLogs` over a few thousand blocks can legitimately take seconds, so
/// this sits above the expected worst case.
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Deadline for a `call`, across every attempt it makes.
///
/// One second under the router's 12s, so a spent budget surfaces as this
/// service's own error rather than as a cancelled future. Two attempts at
/// [`REQUEST_TIMEOUT`] apiece would be 16s, which the router would cut off
/// mid-flight — after the fallback had already opened a socket and, on a paid
/// endpoint, spent a credit for an answer nobody would receive.
pub(super) const TOTAL_BUDGET: Duration = Duration::from_secs(11);

/// Least budget worth starting a fallback attempt with. Below this the attempt
/// cannot finish, so making it only spends money.
pub(super) const MIN_ATTEMPT: Duration = Duration::from_secs(2);

/// Connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Sequential-failover HTTP upstream.
pub struct HttpUpstream {
    endpoints: Vec<Endpoint>,
    http: reqwest::Client,
    chain_id: u64,
    /// Ceiling on calls in flight against this chain.
    ///
    /// Nothing else bounds them: the credit guard permits a burst of
    /// `upstream_burst_units`, which at the `eth_getLogs` weight is around a
    /// hundred concurrent calls, and each is a socket and a buffered response.
    /// Waiting for a permit spends the caller's own deadline, so overload
    /// surfaces as this service's 503 rather than as an unbounded fan-out at a
    /// provider that meters us.
    inflight: tokio::sync::Semaphore,
}

impl HttpUpstream {
    pub fn new(chain_id: u64, endpoints: Vec<Endpoint>, max_inflight: usize) -> AppResult<Self> {
        if endpoints.is_empty() {
            return Err(AppError::Internal(format!(
                "chain {chain_id}: no upstream endpoints"
            )));
        }
        if max_inflight == 0 {
            return Err(AppError::Internal(format!(
                "chain {chain_id}: upstream_max_inflight must be above zero"
            )));
        }
        // One client, so every endpoint shares a connection pool. A second
        // client against the same host would open a second pool for no gain.
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            // Matched to the concurrency cap: more idle sockets than calls that
            // can be in flight is memory held for connections nothing can use.
            .pool_max_idle_per_host(max_inflight)
            .build()
            .map_err(|e| AppError::Internal(format!("build http client: {e}")))?;
        Ok(Self {
            endpoints,
            http,
            chain_id,
            inflight: tokio::sync::Semaphore::new(max_inflight),
        })
    }

    /// One attempt against one endpoint, bounded by whatever is left of the
    /// call's budget.
    async fn attempt(
        &self,
        ep: &Endpoint,
        body: &Body<'_>,
        budget: Duration,
    ) -> Result<Vec<Answer>, Attempt> {
        let res = self
            .http
            .post(&ep.url)
            .timeout(budget)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    // Not retried elsewhere: the request was slow, and a second
                    // slow attempt would run past the client's own deadline.
                    Attempt::Timeout
                } else {
                    Attempt::Transport(describe(e))
                }
            })?;

        let status = res.status();
        if !status.is_success() {
            // 429 and 5xx may succeed elsewhere. A 4xx is a malformed request
            // on our side and would fail identically at any endpoint.
            return Err(if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                Attempt::Throttled {
                    retry_after_secs: retry_after(res.headers()),
                }
            } else if status.is_server_error() {
                Attempt::Transport(format!("http {status}"))
            } else {
                Attempt::Fatal(format!("http {status}"))
            });
        }

        let text = res
            .text()
            .await
            .map_err(|e| Attempt::Transport(describe(e)))?;

        body.decode(&text).map_err(Attempt::Fatal)
    }
}

/// A transport error, safe to log.
///
/// The endpoint URL carries the paid API key, and reqwest puts it in every
/// error's text — so it is stripped here, where the error is made, rather than
/// trusted to every place that might print one. The causes reqwest keeps
/// behind `source()` are kept: "error sending request" alone diagnoses nothing,
/// while "connection refused" or a TLS failure is the whole answer.
pub(super) fn describe(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut out = e.to_string();
    let mut cause = std::error::Error::source(&e);
    while let Some(c) = cause {
        out.push_str(": ");
        out.push_str(&c.to_string());
        cause = c.source();
    }
    out
}

#[async_trait]
impl Upstream for HttpUpstream {
    fn permits_available(&self) -> usize {
        self.inflight.available_permits()
    }

    async fn call(
        &self,
        calls: &[OutboundCall<'_>],
        needs_archive: bool,
    ) -> AppResult<Vec<Answer>> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let body = Body::new(calls);

        let usable: Vec<&Endpoint> = self
            .endpoints
            .iter()
            .filter(|e| !needs_archive || e.archive)
            .collect();

        if usable.is_empty() {
            // A distinct error rather than a generic 502: this is a
            // provisioning state, and it degrades one feature (the earned
            // column) rather than the service.
            return Err(AppError::NoArchiveUpstream);
        }

        // Acquired before the budget starts, so time spent queueing is the
        // caller's own deadline rather than something charged to the upstream.
        // The semaphore is never closed, so this cannot fail.
        let _permit = self
            .inflight
            .acquire()
            .await
            .map_err(|_| AppError::Internal("upstream concurrency limiter closed".into()))?;

        let started = std::time::Instant::now();
        let deadline = started + TOTAL_BUDGET;
        let mut last = AppError::Upstream("no endpoint attempted".into());
        let mut attempts = 0;
        // The soonest any endpoint said it would take a call again. Reported
        // even when a later endpoint failed some other way: the quota is why
        // the call failed, and it is the one failure a client can wait out.
        let mut throttled: Option<u64> = None;
        for ep in usable {
            // An attempt that cannot finish inside what is left would be
            // cancelled by the router anyway, having spent a paid credit on an
            // answer nobody receives.
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left < MIN_ATTEMPT {
                break;
            }
            attempts += 1;
            match self.attempt(ep, &body, left.min(REQUEST_TIMEOUT)).await {
                Ok(answers) => {
                    metrics::counter!(
                        shared::metrics::name::RPC_PROXY_UPSTREAM_CALLS,
                        "chain" => self.chain_id.to_string(),
                        "upstream" => ep.label,
                        "outcome" => "ok",
                    )
                    .increment(1);
                    return Ok(answers);
                }
                Err(a) => {
                    metrics::counter!(
                        shared::metrics::name::RPC_PROXY_UPSTREAM_CALLS,
                        "chain" => self.chain_id.to_string(),
                        "upstream" => ep.label,
                        "outcome" => a.outcome(),
                    )
                    .increment(1);
                    // Per attempt at DEBUG: one that a fallback recovers is
                    // counted by the metric above, and one that is not is in
                    // the WARN below. The detail carries no URL; see
                    // `describe`. It still never reaches a response body.
                    debug!(chain_id = self.chain_id, upstream = ep.label, detail = %a.detail(), "upstream attempt failed");
                    if let Attempt::Throttled { retry_after_secs } = a {
                        throttled =
                            Some(throttled.map_or(retry_after_secs, |t| t.min(retry_after_secs)));
                    }
                    let fatal = a.is_fatal();
                    last = a.into_error();
                    if fatal {
                        break;
                    }
                }
            }
        }
        if let Some(retry_after_secs) = throttled {
            last = AppError::UpstreamThrottled { retry_after_secs };
        }
        warn!(
            chain_id = self.chain_id,
            class = last.class(),
            error = %last,
            calls = calls.len(),
            archive = needs_archive,
            attempts,
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "upstream call failed"
        );
        Err(last)
    }
}

/// How one attempt failed.
enum Attempt {
    /// No answer; another endpoint may succeed.
    Transport(String),
    /// HTTP 429: the endpoint refused for being over our quota, and said how
    /// long to wait — or did not, and [`retry_after`] chose. Another endpoint,
    /// on another quota, may still answer.
    Throttled { retry_after_secs: u64 },
    /// No answer, and slow. Not retried — see [`REQUEST_TIMEOUT`].
    Timeout,
    /// A failure that would repeat identically at any endpoint.
    Fatal(String),
}

impl Attempt {
    fn outcome(&self) -> &'static str {
        match self {
            Attempt::Timeout => "timeout",
            Attempt::Throttled { .. } => "throttled",
            Attempt::Transport(_) | Attempt::Fatal(_) => "error",
        }
    }

    fn is_fatal(&self) -> bool {
        matches!(self, Attempt::Timeout | Attempt::Fatal(_))
    }

    fn detail(&self) -> String {
        match self {
            Attempt::Transport(d) | Attempt::Fatal(d) => d.clone(),
            Attempt::Throttled { retry_after_secs } => {
                format!("http 429, retry after {retry_after_secs}s")
            }
            Attempt::Timeout => "timed out".into(),
        }
    }

    fn into_error(self) -> AppError {
        match self {
            Attempt::Timeout => AppError::UpstreamTimeout,
            Attempt::Throttled { retry_after_secs } => {
                AppError::UpstreamThrottled { retry_after_secs }
            }
            Attempt::Transport(d) | Attempt::Fatal(d) => AppError::Upstream(d),
        }
    }
}

/// Shortest wait passed on to a client for an upstream 429, and what one
/// without a usable `Retry-After` is given.
pub(super) const MIN_RETRY_AFTER_SECS: u64 = 1;

/// Longest. A provider's "come back in an hour" would otherwise put every
/// wallet to sleep on reads the next second's quota, or a fallback, might
/// serve.
pub(super) const MAX_RETRY_AFTER_SECS: u64 = 60;

/// A provider's `Retry-After` in whole seconds, clamped to
/// [`MIN_RETRY_AFTER_SECS`]..=[`MAX_RETRY_AFTER_SECS`].
///
/// Only the delta-seconds form is read. An HTTP-date, or no header at all,
/// reads as the minimum: the client still backs off, just not for a length
/// this service would be guessing.
pub(super) fn retry_after(headers: &reqwest::header::HeaderMap) -> u64 {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(MIN_RETRY_AFTER_SECS)
        .clamp(MIN_RETRY_AFTER_SECS, MAX_RETRY_AFTER_SECS)
}
