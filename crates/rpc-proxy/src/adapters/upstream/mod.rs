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

mod body;
mod http;

pub use http::HttpUpstream;

use crate::domain::error::AppResult;
use async_trait::async_trait;
use serde_json::Value;
use serde_json::value::RawValue;

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

#[cfg(test)]
mod tests;
