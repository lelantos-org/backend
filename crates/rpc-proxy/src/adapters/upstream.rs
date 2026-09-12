//! HTTP client for the paid RPC endpoints.
//!
//! Forwards opaque JSON-RPC bodies and reports which endpoint answered, so
//! per-upstream failure rates are observable.
//!
//! Failover distinguishes two cases:
//!
//! - A transport failure yielded no answer; the next endpoint is tried.
//! - A JSON-RPC error response is an answer. `execution reverted` is a verdict
//!   about the call, so it is returned as-is rather than retried elsewhere.

use crate::domain::error::{AppError, AppResult};
use crate::domain::jsonrpc::{UpstreamResponse, VERSION};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use serde_json::value::RawValue;
use std::time::Duration;
use tracing::{debug, warn};

/// Deadline for one upstream attempt.
///
/// Below the registry's 30s, which is tuned for archive-wide APY reads. An
/// `eth_getLogs` over a few thousand blocks can legitimately take seconds, so
/// this sits above the expected worst case.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Deadline for a `call`, across every attempt it makes.
///
/// One second under the router's 12s, so a spent budget surfaces as this
/// service's own error rather than as a cancelled future. Two attempts at
/// [`REQUEST_TIMEOUT`] apiece would be 16s, which the router would cut off
/// mid-flight — after the fallback had already opened a socket and, on a paid
/// endpoint, spent a credit for an answer nobody would receive.
const TOTAL_BUDGET: Duration = Duration::from_secs(11);

/// Least budget worth starting a fallback attempt with. Below this the attempt
/// cannot finish, so making it only spends money.
const MIN_ATTEMPT: Duration = Duration::from_secs(2);

/// Connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// One call to forward. The client's own `id` is not carried: ids are assigned
/// per outbound batch and mapped back by the handler, so a client cannot use a
/// duplicated or hostile id to confuse response matching.
#[derive(Debug, Clone, Copy)]
pub struct OutboundCall<'a> {
    pub method: &'a str,
    pub params: Option<&'a Value>,
}

/// What one call produced.
///
/// An `Error` here is an upstream verdict passed through untouched, not a
/// failure of this service.
#[derive(Debug)]
pub enum Answer {
    Result(Box<RawValue>),
    Error(Value),
}

/// The upstream RPC for one chain.
#[async_trait]
pub trait Upstream: Send + Sync {
    /// Forward `calls` as a single batch and return one answer per call, in the
    /// order given.
    ///
    /// `needs_archive` restricts the attempt to endpoints that retain historical
    /// state. A pruning fallback would answer a historical read with a JSON-RPC
    /// error, which by the rule above is a verdict — so it would be cached and
    /// returned as though the chain had said it, rather than falling through.
    async fn call(&self, calls: &[OutboundCall<'_>], needs_archive: bool)
    -> AppResult<Vec<Answer>>;

    /// Free slots in this upstream's concurrency cap, for the gauge.
    ///
    /// On the trait so the metric does not need a downcast. An implementation
    /// with no such cap reports its own ceiling.
    fn permits_available(&self) -> usize;
}

/// One configured endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub url: String,
    /// Whether this endpoint retains historical state.
    pub archive: bool,
    /// `primary` or `fallback` — the metric label.
    ///
    /// Never the URL: an endpoint URL here carries the paid API key, and a
    /// metric label is scraped, stored and rendered on dashboards.
    pub label: &'static str,
}

pub const PRIMARY: &str = "primary";
pub const FALLBACK: &str = "fallback";

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
fn describe(e: reqwest::Error) -> String {
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
const MIN_RETRY_AFTER_SECS: u64 = 1;

/// Longest. A provider's "come back in an hour" would otherwise put every
/// wallet to sleep on reads the next second's quota, or a fallback, might
/// serve.
const MAX_RETRY_AFTER_SECS: u64 = 60;

/// A provider's `Retry-After` in whole seconds, clamped to
/// [`MIN_RETRY_AFTER_SECS`]..=[`MAX_RETRY_AFTER_SECS`].
///
/// Only the delta-seconds form is read. An HTTP-date, or no header at all,
/// reads as the minimum: the client still backs off, just not for a length
/// this service would be guessing.
fn retry_after(headers: &reqwest::header::HeaderMap) -> u64 {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(MIN_RETRY_AFTER_SECS)
        .clamp(MIN_RETRY_AFTER_SECS, MAX_RETRY_AFTER_SECS)
}

/// The outbound request, and the shape its response must have.
///
/// A one-call batch is sent as a bare object rather than a single-element
/// array: it is what every client sends for a lone call, and some providers
/// meter or handle the two paths differently.
#[derive(Serialize)]
#[serde(untagged)]
enum Body<'a> {
    Single(Envelope<'a>),
    Batch(Vec<Envelope<'a>>),
}

#[derive(Serialize)]
struct Envelope<'a> {
    jsonrpc: &'static str,
    id: usize,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a Value>,
}

impl<'a> Body<'a> {
    fn new(calls: &'a [OutboundCall<'a>]) -> Self {
        let mut envelopes: Vec<Envelope<'a>> = calls
            .iter()
            .enumerate()
            .map(|(i, c)| Envelope {
                jsonrpc: VERSION,
                id: i,
                method: c.method,
                params: c.params,
            })
            .collect();

        if envelopes.len() == 1 {
            Body::Single(envelopes.pop().expect("length checked"))
        } else {
            Body::Batch(envelopes)
        }
    }

    fn len(&self) -> usize {
        match self {
            Body::Single(_) => 1,
            Body::Batch(v) => v.len(),
        }
    }

    /// Parse a response and put the answers back in request order.
    ///
    /// Reordering is required, not defensive: JSON-RPC explicitly permits a
    /// server to return batch responses in any order. Zipping by position would
    /// hand one caller another caller's answer — the worst failure this service
    /// could have, since both are valid JSON and nothing downstream would
    /// notice.
    fn decode(&self, text: &str) -> Result<Vec<Answer>, String> {
        let n = self.len();
        let raw: Vec<UpstreamResponse> = match self {
            Body::Single(_) => vec![
                serde_json::from_str(text).map_err(|e| format!("decode single response: {e}"))?,
            ],
            Body::Batch(_) => {
                serde_json::from_str(text).map_err(|e| format!("decode batch response: {e}"))?
            }
        };
        if raw.len() != n {
            return Err(format!("expected {n} responses, got {}", raw.len()));
        }

        let mut slots: Vec<Option<Answer>> = (0..n).map(|_| None).collect();
        for r in raw {
            let idx = match self {
                // A lone call has one slot; a server that echoed a different id
                // is still answering the only question asked.
                Body::Single(_) => 0,
                Body::Batch(_) => {
                    r.id.as_u64()
                        .and_then(|i| usize::try_from(i).ok())
                        .filter(|i| *i < n)
                        .ok_or_else(|| "batch response carried an unknown id".to_string())?
                }
            };
            let answer = match (r.result, r.error) {
                // `result` wins if a server sends both, which is malformed
                // anyway; taking the error would turn a good answer into a
                // failure.
                (Some(res), _) => Answer::Result(res),
                (None, Some(err)) => Answer::Error(err),
                (None, None) => return Err("response carried neither result nor error".into()),
            };
            if slots[idx].replace(answer).is_some() {
                return Err("batch response repeated an id".into());
            }
        }

        slots
            .into_iter()
            .map(|s| s.ok_or_else(|| "batch response omitted an id".to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The API key lives in the endpoint URL, and reqwest prints the URL in
    /// its errors. What is logged must keep the cause and lose the key.
    #[tokio::test]
    async fn a_transport_error_is_described_without_its_url() {
        // Port 1 on loopback: refused at once, with no network involved.
        let e = reqwest::Client::new()
            .post("http://127.0.0.1:1/v2/SECRETAPIKEY")
            .send()
            .await
            .unwrap_err();
        assert!(
            e.to_string().contains("SECRETAPIKEY"),
            "precondition: reqwest does print the URL"
        );

        let d = describe(e);
        assert!(!d.contains("SECRETAPIKEY"), "{d}");
        assert!(!d.contains("127.0.0.1:1/v2"), "{d}");
        assert!(d.contains(": "), "the underlying cause is kept: {d}");
    }

    fn calls(n: usize) -> Vec<OutboundCall<'static>> {
        (0..n)
            .map(|_| OutboundCall {
                method: "eth_blockNumber",
                params: None,
            })
            .collect()
    }

    /// A lone call goes out as an object, not a one-element array.
    #[test]
    fn a_single_call_is_not_sent_as_a_batch() {
        let c = calls(1);
        let text = serde_json::to_string(&match Body::new(&c) {
            Body::Single(e) => e,
            Body::Batch(_) => panic!("should be single"),
        })
        .unwrap();
        assert!(text.starts_with('{'), "{text}");
        assert!(text.contains(r#""jsonrpc":"2.0""#), "{text}");
    }

    /// The correctness case that matters most here. JSON-RPC lets a server
    /// answer a batch in any order; zipping by position would hand one caller
    /// another's answer, and both would be valid JSON.
    #[test]
    fn a_batch_response_is_reordered_by_id() {
        let c = calls(3);
        let body = Body::new(&c);
        let answers = body
            .decode(
                &json!([
                    {"jsonrpc":"2.0","id":2,"result":"0xcc"},
                    {"jsonrpc":"2.0","id":0,"result":"0xaa"},
                    {"jsonrpc":"2.0","id":1,"result":"0xbb"}
                ])
                .to_string(),
            )
            .unwrap();

        let got: Vec<String> = answers
            .iter()
            .map(|a| match a {
                Answer::Result(r) => r.get().to_string(),
                Answer::Error(_) => panic!("unexpected error"),
            })
            .collect();
        assert_eq!(got, vec![r#""0xaa""#, r#""0xbb""#, r#""0xcc""#]);
    }

    /// An upstream error is an answer, carried through in its own slot rather
    /// than failing the whole batch: one reverting `eth_call` must not take the
    /// other nineteen reads down with it.
    #[test]
    fn one_error_in_a_batch_does_not_fail_the_others() {
        let c = calls(2);
        let answers = Body::new(&c)
            .decode(
                &json!([
                    {"jsonrpc":"2.0","id":0,"error":{"code":3,"message":"execution reverted"}},
                    {"jsonrpc":"2.0","id":1,"result":"0xbb"}
                ])
                .to_string(),
            )
            .unwrap();

        assert!(matches!(answers[0], Answer::Error(_)));
        assert!(matches!(answers[1], Answer::Result(_)));
    }

    /// A response set that cannot be matched to the requests is rejected rather
    /// than guessed at.
    #[test]
    fn a_mismatched_batch_response_is_an_error() {
        let c = calls(2);
        let body = Body::new(&c);

        for bad in [
            // Too few.
            json!([{"jsonrpc":"2.0","id":0,"result":"0xaa"}]),
            // An id outside the batch.
            json!([
                {"jsonrpc":"2.0","id":0,"result":"0xaa"},
                {"jsonrpc":"2.0","id":9,"result":"0xbb"}
            ]),
            // The same id twice, leaving a slot unfilled.
            json!([
                {"jsonrpc":"2.0","id":0,"result":"0xaa"},
                {"jsonrpc":"2.0","id":0,"result":"0xbb"}
            ]),
            // Neither result nor error.
            json!([
                {"jsonrpc":"2.0","id":0},
                {"jsonrpc":"2.0","id":1,"result":"0xbb"}
            ]),
        ] {
            assert!(body.decode(&bad.to_string()).is_err(), "must reject {bad}");
        }
    }

    /// A `null` result is a real answer — an unmined receipt — and must survive
    /// decoding rather than being read as a missing field.
    #[test]
    fn a_null_result_decodes_as_an_answer() {
        let c = calls(1);
        let answers = Body::new(&c)
            .decode(&json!({"jsonrpc":"2.0","id":0,"result":null}).to_string())
            .unwrap();
        match &answers[0] {
            Answer::Result(r) => assert_eq!(r.get(), "null"),
            Answer::Error(_) => panic!("null result is not an error"),
        }
    }

    /// With no archive endpoint configured, a historical read must report that
    /// specifically rather than as a generic upstream failure.
    #[tokio::test]
    async fn an_archive_read_with_no_archive_endpoint_is_reported_as_such() {
        let up = HttpUpstream::new(
            1,
            vec![Endpoint {
                url: "http://127.0.0.1:1/".into(),
                archive: false,
                label: PRIMARY,
            }],
            64,
        )
        .unwrap();

        let err = up.call(&calls(1), true).await.unwrap_err();
        assert!(matches!(err, AppError::NoArchiveUpstream), "{err}");
    }

    #[test]
    fn an_upstream_with_no_endpoints_is_a_config_error() {
        assert!(HttpUpstream::new(1, vec![], 64).is_err());
    }

    /// A cap of zero would make every call block forever, which looks like a
    /// hung upstream rather than a config mistake.
    #[test]
    fn a_zero_concurrency_cap_is_a_config_error() {
        let ep = vec![Endpoint {
            url: "http://127.0.0.1:1/".into(),
            archive: true,
            label: PRIMARY,
        }];
        assert!(HttpUpstream::new(1, ep, 0).is_err());
    }

    fn endpoint(url: String) -> Vec<Endpoint> {
        vec![Endpoint {
            url,
            archive: true,
            label: PRIMARY,
        }]
    }

    /// An endpoint that refuses every request with HTTP 429 and this
    /// `Retry-After`, as a provider over our quota does. Returns its URL.
    async fn throttling(retry_after: &'static str) -> String {
        let app = axum::Router::new().fallback(move || async move {
            (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", retry_after)],
            )
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}/")
    }

    /// The provider said how long; the client is told the same.
    #[tokio::test]
    async fn an_upstream_429_is_throttled_with_its_retry_after() {
        let url = throttling("7").await;
        let up = HttpUpstream::new(1, endpoint(url), 1).unwrap();
        let err = up.call(&calls(1), false).await.unwrap_err();
        assert!(
            matches!(
                err,
                AppError::UpstreamThrottled {
                    retry_after_secs: 7
                }
            ),
            "{err:?}"
        );
    }

    /// The quota is why the call failed even when the fallback then failed
    /// some other way, and it is the failure a client can wait out.
    #[tokio::test]
    async fn a_throttled_primary_is_reported_over_a_failed_fallback() {
        let url = throttling("5").await;
        let endpoints = vec![
            Endpoint {
                url,
                archive: true,
                label: PRIMARY,
            },
            Endpoint {
                // Refused at once.
                url: "http://127.0.0.1:1/".into(),
                archive: true,
                label: FALLBACK,
            },
        ];
        let up = HttpUpstream::new(1, endpoints, 1).unwrap();
        let err = up.call(&calls(1), false).await.unwrap_err();
        assert!(
            matches!(
                err,
                AppError::UpstreamThrottled {
                    retry_after_secs: 5
                }
            ),
            "{err:?}"
        );
    }

    /// Read in whole seconds, and clamped so neither "now" nor "an hour" is
    /// passed on.
    #[test]
    fn retry_after_is_read_and_clamped() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

        let with = |v: &'static str| {
            let mut h = HeaderMap::new();
            h.insert(RETRY_AFTER, HeaderValue::from_static(v));
            retry_after(&h)
        };
        assert_eq!(with("7"), 7);
        assert_eq!(with("0"), MIN_RETRY_AFTER_SECS);
        assert_eq!(with("3600"), MAX_RETRY_AFTER_SECS);
        assert_eq!(
            with("Wed, 21 Oct 2015 07:28:00 GMT"),
            MIN_RETRY_AFTER_SECS,
            "an HTTP-date is not guessed at"
        );
        assert_eq!(retry_after(&HeaderMap::new()), MIN_RETRY_AFTER_SECS);
    }

    /// The whole point of the cap: a call in flight holds a permit, so the ones
    /// past the cap wait rather than opening another socket at a provider that
    /// meters us.
    ///
    /// Driven by a listener that accepts and never answers, rather than by a
    /// closed port — a refused connection fails immediately and the call would
    /// be over before it could be observed.
    #[tokio::test]
    async fn an_in_flight_call_holds_a_permit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Held, not dropped: closing the socket would answer the request
            // with a transport error and end the call.
            let mut open = Vec::new();
            while let Ok((s, _)) = listener.accept().await {
                open.push(s);
            }
        });

        let up = std::sync::Arc::new(
            HttpUpstream::new(1, endpoint(format!("http://{addr}/")), 1).unwrap(),
        );
        assert_eq!(up.permits_available(), 1);

        let bg = std::sync::Arc::clone(&up);
        tokio::spawn(async move {
            let c = calls(1);
            let _ = bg.call(&c, false).await;
        });

        for _ in 0..200 {
            if up.permits_available() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("an in-flight call must hold the only permit");
    }

    /// A permit that is not returned wedges the service one slot at a time, and
    /// the failure path is where that would happen — it is the one that does
    /// not run to the end of the function.
    #[tokio::test]
    async fn a_failed_call_returns_its_permit() {
        // Port 1 refuses, so each call fails fast.
        let up = HttpUpstream::new(1, endpoint("http://127.0.0.1:1/".into()), 4).unwrap();

        for _ in 0..8 {
            let c = calls(1);
            assert!(up.call(&c, false).await.is_err());
        }

        assert_eq!(
            up.permits_available(),
            4,
            "a permit must not leak on the failure path"
        );
    }

    /// The two timeouts have to compose to less than the router's deadline, or
    /// the fallback attempt is cancelled mid-flight — after it has already
    /// opened a socket and spent a paid credit on an answer nobody receives.
    #[test]
    fn the_call_budget_fits_inside_the_request_deadline() {
        // `handlers::http::router::REQUEST_TIMEOUT`.
        assert!(
            TOTAL_BUDGET < Duration::from_secs(12),
            "the call budget must leave the router room to answer"
        );
        assert!(
            MIN_ATTEMPT < REQUEST_TIMEOUT && REQUEST_TIMEOUT <= TOTAL_BUDGET,
            "one full attempt must fit in the budget"
        );
    }
}
