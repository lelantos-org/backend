//! GCRA rate limiting, charged in compute units.
//!
//! Requests are unauthenticated, so the key is the client IP. Two consequences
//! follow.
//!
//! IPv6 is bucketed by /64 and IPv4 by /32. A residential IPv6 client holds a
//! /64 or larger, so per-address limiting is defeated at no cost to the client.
//!
//! A per-IP limit does not bound a distributed caller. The per-chain global
//! bucket bounds the credit budget; the per-IP buckets bound any one client's
//! share of it.
//!
//! Charging happens before the cache is consulted, since the limiter also
//! bounds this service's own sockets and CPU. An honest wallet uses roughly a
//! tenth of its budget.
//!
//! It happens in two stages, because the two costs become knowable at different
//! points. [`ClientLimiter::admit`] charges for the *body* before it is parsed,
//! which is the only thing standing in front of the deserialization itself;
//! the method weights are charged once the parse has said what was asked for.
//!
//! The per-client buckets are shared across chains and the credit guard is per
//! chain, which is how the two config blocks are already shaped — see
//! [`ClientLimiter`] and [`GlobalLimiter`].

mod client;
mod limiter;

pub use client::{ClientKey, HeaderPosition, TrustedHeader, client_key};
pub use limiter::{ClientLimiter, ClientQuotas, GlobalLimiter, GlobalQuotas};

use crate::domain::error::{AppError, AppResult};
use shared::metrics::name;

/// Which bucket refused a request. A closed set, and the metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    IpShort,
    IpLong,
    Global,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::IpShort => "ip_short",
            Scope::IpLong => "ip_long",
            Scope::Global => "global",
        }
    }
}

/// The verdict for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny {
        scope: Scope,
        /// Whole seconds until the request would succeed, rounded up. Never
        /// zero: telling a client to retry immediately is the one answer that
        /// makes a burst worse.
        retry_after_secs: u64,
    },
    /// The request weighs more than the bucket's whole burst, so no amount of
    /// waiting lets it through. Distinct from [`Decision::Deny`] because the
    /// right answer differs: not "retry later" but "send less at once".
    Never {
        scope: Scope,
        units: u32,
        capacity: u32,
    },
}

impl Decision {
    /// Count a denial and turn it into the caller's error.
    ///
    /// Here rather than at each call site so the `scope` counter cannot drift
    /// from the 429 it explains — every refusal is counted exactly once, by the
    /// bucket that refused it.
    pub fn into_result(self) -> AppResult<()> {
        let (scope, error) = match self {
            Decision::Allow => return Ok(()),
            Decision::Deny {
                scope,
                retry_after_secs,
            } => (scope, AppError::RateLimited { retry_after_secs }),
            Decision::Never {
                scope,
                units,
                capacity,
            } => (
                scope,
                AppError::TooCostly {
                    units,
                    limit: capacity,
                },
            ),
        };
        metrics::counter!(name::RPC_PROXY_RATE_LIMITED, "scope" => scope.label()).increment(1);
        Err(error)
    }
}

#[cfg(test)]
mod tests;
