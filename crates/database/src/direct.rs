//! Connection URL for sessions that must bypass a connection pooler.
//!
//! Three things in this crate hold Postgres session state across statements,
//! each on a connection deliberately kept out of the bb8 pool:
//!
//!   - [`crate::advisory`] holds a session-level `pg_try_advisory_lock` for the
//!     life of the process (per-chain leader election)
//!   - [`crate::listen`] holds a `LISTEN` subscription
//!   - [`crate::migrate`] runs DDL under the advisory migration lock
//!
//! A transaction pooler multiplexes many clients onto a shared set of server
//! connections, so all three are only correct if it recognises the session
//! state and pins the connection. PgDog does pin advisory locks in practice,
//! and proxies `LISTEN` over a dedicated connection of its own, but neither is
//! a guarantee this crate should depend on: when a pooler gets it wrong it does
//! not raise an error, it hands two indexers the same chain and lets both
//! write.
//!
//! So these three resolve their URL through [`url`] instead. The deployment
//! points `DATABASE_URL` at the pooler and `DATABASE_DIRECT_URL` at Postgres:
//! pooled repository traffic is multiplexed, and the session-scoped
//! connections keep the semantics they were written against.
//!
//! Resolution lives in the three functions rather than in each service's
//! config, so a new caller of `ChainLock`, `listen::spawn` or `migrate::run`
//! cannot forget it.
//!
//! Unset, this is the identity function: tests and any deployment with no
//! pooler in front of Postgres behave exactly as before.

/// Name of the override.
pub const ENV_DIRECT_URL: &str = "DATABASE_DIRECT_URL";

/// `DATABASE_DIRECT_URL` when set to a non-empty value, else `pooled` unchanged.
///
/// Read on each call rather than cached once: the three callers resolve it a
/// handful of times per process, and a cache would make the value depend on
/// which of them ran first.
///
/// Blank is treated as unset. An unset Ansible variable renders as the empty
/// string, and connecting to `""` fails at startup with a URL parse error that
/// reads nothing like "the operator left a variable undefined".
pub fn url(pooled: &str) -> String {
    match std::env::var(ENV_DIRECT_URL) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => pooled.to_string(),
    }
}
