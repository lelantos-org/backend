//! The GCRA buckets: per client, shared across chains, and per chain.

use super::{ClientKey, Decision, Scope};
use crate::domain::error::AppResult;
use governor::clock::{Clock, DefaultClock, Reference};
use governor::middleware::NoOpMiddleware;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::state::{InMemoryState, NotKeyed};
use governor::{InsufficientCapacity, NotUntil, Quota, RateLimiter};
use std::num::NonZeroU32;
use std::time::Duration;

/// Quotas for the per-client buckets.
///
/// One set for the whole service rather than one per chain: see
/// [`ClientLimiter`].
#[derive(Debug, Clone, Copy)]
pub struct ClientQuotas {
    /// Sustained per-client rate, in units per second.
    pub units_per_second: u32,
    /// How far a client may run ahead of that rate. Sized to absorb a wallet's
    /// boot burst (~48 units per chain) without touching it.
    pub burst_units: u32,
    /// The longer per-client ceiling, which is the one that actually bounds an
    /// abuser.
    pub long_units: u32,
    pub long_window: Duration,
}

/// Quotas for one chain's credit guard.
#[derive(Debug, Clone, Copy)]
pub struct GlobalQuotas {
    /// The whole chain's budget, which per-IP limits cannot bound because an
    /// attacker can hold many IPs.
    pub units_per_second: u32,
    pub burst_units: u32,
}

/// Bytes of request body per unit of admission charge. See
/// [`ClientLimiter::admit`].
const ADMISSION_BYTES_PER_UNIT: usize = 32 * 1024;

/// What accepting a body of `len` costs before it has been looked at.
///
/// One unit as a floor so no request is free, plus one per 32 KB so a large
/// body costs proportionally more to have offered. At the 256 KB body cap this
/// is 9 units, against an `eth_getLogs` weight of 15 — expensive enough that a
/// flood of oversized garbage is bounded by the same budget real traffic
/// spends, and cheap enough that an honest 3 KB batch pays 1.
pub(super) fn admission_units(len: usize) -> u32 {
    1 + u32::try_from(len / ADMISSION_BYTES_PER_UNIT).unwrap_or(u32::MAX - 1)
}

/// A per-client bucket. The middleware parameter has to be named explicitly:
/// its default is pinned to the quanta clock, which the tests replace.
type Keyed<C> = RateLimiter<
    ClientKey,
    DefaultKeyedStateStore<ClientKey>,
    C,
    NoOpMiddleware<<C as Clock>::Instant>,
>;

/// The chain-wide bucket.
type Direct<C> = RateLimiter<NotKeyed, InMemoryState, C, NoOpMiddleware<<C as Clock>::Instant>>;

/// The two per-client GCRA buckets, shared by every chain.
///
/// One instance for the service, not one per chain. The quotas are a single
/// global config block, so building this per chain would silently multiply a
/// client's budget by the number of configured chains — a three-chain
/// deployment would enforce three times what it documents.
///
/// The keyed stores are dashmaps that only reclaim idle keys when told to, so
/// [`Self::gc`] must be driven periodically; the key is an unauthenticated IP
/// prefix and an abuser can mint them faster than traffic retires them.
pub struct ClientLimiter<C: Clock = DefaultClock> {
    short: Keyed<C>,
    long: Keyed<C>,
}

impl<C> ClientLimiter<C>
where
    C: Clock + Clone + 'static,
    C::Instant: 'static,
{
    pub fn new(q: ClientQuotas, clock: C) -> Result<Self, String> {
        let short = quota(q.units_per_second, q.burst_units, "client")?;
        let long = window_quota(q.long_units, q.long_window)?;
        Ok(Self {
            short: RateLimiter::dashmap_with_clock(short, clock.clone()),
            long: RateLimiter::dashmap_with_clock(long, clock),
        })
    }

    /// Charge `units` against both per-client buckets.
    pub fn check(&self, key: ClientKey, units: u32) -> Decision {
        // Zero-weight calls (`eth_chainId`) are answered from config and cost
        // nothing anywhere, so they are not charged.
        let Some(n) = NonZeroU32::new(units) else {
            return Decision::Allow;
        };
        let now = self.now();

        for (scope, outcome) in [
            (Scope::IpShort, self.short.check_key_n(&key, n)),
            (Scope::IpLong, self.long.check_key_n(&key, n)),
        ] {
            match decide(scope, outcome, now, units) {
                Decision::Allow => {}
                deny => return deny,
            }
        }
        Decision::Allow
    }

    /// Charge for accepting a body of `len` bytes, before it is parsed.
    ///
    /// Everything the method weights charge for is known only after the body has
    /// been deserialized — and that deserialization is itself the most expensive
    /// thing an unauthenticated caller can ask this process to do, since the body
    /// cap is 256 KB. Without a charge here a flood of oversized garbage is
    /// answered with a parse error at no cost to the sender, on a connection it
    /// can reuse immediately.
    ///
    /// Charged against the per-client buckets only. The chain-wide bucket guards
    /// the *upstream credit* budget, and a body that never parses spends none.
    ///
    /// Returns the caller's error rather than a [`Decision`]: nothing here is
    /// yet parsed, so there is no per-method counting for the caller to do and
    /// nothing to decide.
    pub fn admit(&self, key: ClientKey, len: usize) -> AppResult<()> {
        self.check(key, admission_units(len)).into_result()
    }

    /// Drop buckets that have been idle long enough to be back at full capacity.
    ///
    /// `governor` never does this on its own: every distinct key seen since
    /// start otherwise stays in the dashmap for the life of the process. With
    /// IPv6 keyed by /64, an attacker holding a /48 mints 65,536 of them, and a
    /// misconfigured trusted header makes it unbounded.
    ///
    /// Retiring a bucket at full capacity gives nothing away — a client that
    /// returns after that long would have had its whole burst regardless.
    pub fn gc(&self) {
        self.short.retain_recent();
        self.long.retain_recent();
        self.short.shrink_to_fit();
        self.long.shrink_to_fit();
    }

    /// Live key count per bucket, for the gauge. Labels are [`Scope::label`],
    /// so the set stays closed.
    pub fn key_counts(&self) -> [(&'static str, usize); 2] {
        [
            (Scope::IpShort.label(), self.short.len()),
            (Scope::IpLong.label(), self.long.len()),
        ]
    }

    fn now(&self) -> C::Instant {
        self.short.clock().now()
    }
}

/// One chain's credit guard.
///
/// Per chain, unlike [`ClientLimiter`]: it bounds spend against that chain's own
/// metered upstream, and the quota comes from that chain's config block.
pub struct GlobalLimiter<C: Clock = DefaultClock> {
    global: Direct<C>,
}

impl<C> GlobalLimiter<C>
where
    C: Clock + Clone + 'static,
    C::Instant: 'static,
{
    pub fn new(q: GlobalQuotas, clock: C) -> Result<Self, String> {
        let global = quota(q.units_per_second, q.burst_units, "upstream")?;
        Ok(Self {
            global: RateLimiter::direct_with_clock(global, clock),
        })
    }

    pub fn check(&self, units: u32) -> Decision {
        let Some(n) = NonZeroU32::new(units) else {
            return Decision::Allow;
        };
        let now = self.global.clock().now();
        decide(Scope::Global, self.global.check_n(n), now, units)
    }
}

/// One bucket's answer, in this module's terms.
///
/// `Err` is `InsufficientCapacity`: the weight exceeds that bucket's whole
/// burst, so it can never succeed however long the caller waits. Reported as
/// [`Decision::Never`] rather than as a denial with some `Retry-After`: a
/// client told to retry would retry a request that fails identically every
/// time, and viem does so three times before giving up.
fn decide<I: Reference>(
    scope: Scope,
    outcome: Result<Result<(), NotUntil<I>>, InsufficientCapacity>,
    now: I,
    units: u32,
) -> Decision {
    match outcome {
        Ok(Ok(())) => Decision::Allow,
        Ok(Err(not_until)) => Decision::Deny {
            scope,
            retry_after_secs: secs(not_until.wait_time_from(now)),
        },
        Err(InsufficientCapacity(capacity)) => Decision::Never {
            scope,
            units,
            capacity,
        },
    }
}

/// A rate with a burst allowance.
fn quota(per_second: u32, burst: u32, what: &str) -> Result<Quota, String> {
    let rate = NonZeroU32::new(per_second)
        .ok_or_else(|| format!("{what}_units_per_second must be above zero"))?;
    let burst =
        NonZeroU32::new(burst).ok_or_else(|| format!("{what}_burst_units must be above zero"))?;
    if burst < rate {
        // GCRA would accept it, but a burst below the sustained rate means the
        // limiter would refuse one second of otherwise-allowed traffic.
        return Err(format!(
            "{what}_burst_units ({burst}) must be at least {what}_units_per_second ({rate})"
        ));
    }
    Ok(Quota::per_second(rate).allow_burst(burst))
}

/// `units` spread evenly over `window`, replenishing continuously rather than
/// resetting on a boundary — so a client cannot spend a whole window's budget
/// twice by straddling the edge.
fn window_quota(units: u32, window: Duration) -> Result<Quota, String> {
    let units =
        NonZeroU32::new(units).ok_or_else(|| "client_long_units must be above zero".to_string())?;
    if window.is_zero() {
        return Err("client_long_window_s must be above zero".into());
    }
    Quota::with_period(window / units.get())
        .map(|q| q.allow_burst(units))
        .ok_or_else(|| "client_long_window_s is too short for that many units".into())
}

/// Whole seconds, rounded up and floored at one.
fn secs(d: Duration) -> u64 {
    d.as_secs().max(u64::from(d.subsec_nanos() > 0)).max(1)
}
