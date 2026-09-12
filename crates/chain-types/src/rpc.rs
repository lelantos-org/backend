//! The JSON-RPC transport every service reaches a chain through.
//!
//! Holds what sits underneath a provider: the parsed URL and a `reqwest::Client`
//! whose clones share a connection pool. Without the shared client every request
//! pays a fresh TCP and TLS handshake.
//!
//! Behind the `rpc` feature, because this crate is otherwise pure data and the
//! indexers that only decode logs must not link a provider to get the ABI.
//!
//! This was duplicated in the relayer and registry-webserver, and absent
//! entirely from three other call sites that built a provider with alloy's
//! default client — which carries **no request timeout at all**. Timeouts are
//! the point of this type, not an incidental: an untimed call against a hung
//! node never returns, and the relayer holds a per-chain mutex across
//! submission, so one such call stalls every spend, swap and flush queued behind
//! it. The deadline belongs to the caller, so it is an argument rather than a
//! constant here.

use alloy::rpc::client::{ClientBuilder, RpcClient};
use alloy::transports::http::Http;
use alloy::transports::http::reqwest::Url;
use alloy::transports::layers::{RetryBackoffLayer, RetryBackoffService};
use std::time::Duration;

/// The transport every provider built here sits on. Named because the retry
/// layer wraps it and a caller holding a provider in a field must spell the type
/// out.
pub type HttpTransport = RetryBackoffService<Http<reqwest::Client>>;

/// Deadline for reaching the node at all, as opposed to hearing back from it.
///
/// Not caller-tunable: this bounds the TCP and TLS handshake, which does not
/// vary with what the call is for.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Retry budget for a call the node failed to answer.
///
/// `RateLimitRetryPolicy` is broader than its name: it retries any
/// transport-level error alloy marks retryable, not only 429s.
///
/// Retrying is safe on every path that uses this. Reads are idempotent, and the
/// one write, `eth_sendRawTransaction`, carries an already-signed transaction,
/// so a resend has the same hash and a node that already holds it answers
/// "already known" rather than broadcasting twice.
///
/// Worst case for one logical call is `request_timeout * (1 + RETRIES)` plus
/// backoff.
const RETRIES: u32 = 3;
const RETRY_BACKOFF_MS: u64 = 200;
/// Paces retries after a rate-limit response, set high enough not to throttle a
/// caller's own call volume.
const COMPUTE_UNITS_PER_SECOND: u64 = 500;

/// How long a single JSON-RPC call may take, per caller.
///
/// A deadline is required, not optional: the whole reason this type exists is
/// that alloy's default client has none.
#[derive(Debug, Clone, Copy)]
pub struct RpcTimeouts {
    pub request: Duration,
}

impl RpcTimeouts {
    pub const fn request(secs: u64) -> Self {
        Self {
            request: Duration::from_secs(secs),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RpcEndpoint {
    url: Url,
    http: reqwest::Client,
}

impl RpcEndpoint {
    /// Errors are strings so each caller maps them into its own error enum;
    /// this crate has no view of what an unreachable node means to the service.
    pub fn new(rpc_url: &str, timeouts: RpcTimeouts) -> Result<Self, String> {
        let url: Url = rpc_url.parse().map_err(|e| format!("rpc url: {e}"))?;
        let http = reqwest::Client::builder()
            .timeout(timeouts.request)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| format!("rpc http client: {e}"))?;
        Ok(Self { url, http })
    }

    /// A fresh RPC client over the shared connection pool. Pass it to
    /// `ProviderBuilder::on_client` once, at construction: the fillers built on
    /// top of it cache per-provider state.
    pub fn client(&self) -> RpcClient<HttpTransport> {
        /// Alloy's `is_local` flag, which only selects a default poll interval:
        /// 250 ms local, 7 s remote. Submitters override it, so leaving it remote
        /// keeps the conservative default for those that do not.
        const IS_LOCAL: bool = false;

        ClientBuilder::default()
            .layer(RetryBackoffLayer::new(
                RETRIES,
                RETRY_BACKOFF_MS,
                COMPUTE_UNITS_PER_SECOND,
            ))
            .transport(
                Http::with_client(self.http.clone(), self.url.clone()),
                IS_LOCAL,
            )
    }
}
