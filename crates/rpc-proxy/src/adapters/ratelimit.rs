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

use crate::domain::error::{AppError, AppResult};
use governor::clock::{Clock, DefaultClock, Reference};
use governor::middleware::NoOpMiddleware;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::state::{InMemoryState, NotKeyed};
use governor::{InsufficientCapacity, NotUntil, Quota, RateLimiter};
use shared::metrics::name;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::time::Duration;

/// What a client is bucketed by.
///
/// A prefix rather than an address, for the reason in the module docs. Held as
/// bytes so v4 and v6 share one key type without either being widened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientKey {
    /// A full IPv4 address.
    V4([u8; 4]),
    /// The first 64 bits of an IPv6 address — one subscriber's allocation.
    V6Prefix([u8; 8]),
}

impl ClientKey {
    pub fn from_ip(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => ClientKey::V4(v4.octets()),
            IpAddr::V6(v6) => {
                // A v4-mapped v6 address is a v4 client arriving over a v6
                // socket; bucketing it as a /64 would give every such client
                // the same key as every other.
                if let Some(v4) = v6.to_ipv4_mapped() {
                    return ClientKey::V4(v4.octets());
                }
                let o = v6.octets();
                ClientKey::V6Prefix(o[..8].try_into().expect("16-byte address"))
            }
        }
    }

    /// A short, non-identifying digest for logs.
    ///
    /// The raw address is never logged. This service observes the read pattern
    /// of every wallet in one place; pairing that with an IP would create a
    /// correlation that does not otherwise exist.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        match self {
            ClientKey::V4(b) => h.update(b),
            ClientKey::V6Prefix(b) => h.update(b),
        }
        hex::encode(&h.finalize()[..6])
    }
}

/// Which entry of a forwarding header to believe.
///
/// This is a security choice, not a formatting one. `X-Forwarded-For` is a list
/// that each hop **appends** to, so the entries a caller sent are on the left
/// and the one the nearest trusted proxy added is on the right. Reading the
/// left end means reading whatever the caller wrote there, which lets anyone
/// pick their own rate-limit bucket — and mint an unbounded number of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeaderPosition {
    /// The last entry: the address the nearest trusted proxy observed.
    ///
    /// The default, and the safe reading of `X-Forwarded-For`, which
    /// Cloudflare appends the visitor's address to.
    ///
    /// Behind Cloudflare the better header is `CF-Connecting-IP`, which
    /// Cloudflare *overwrites* rather than appends — so it carries one entry
    /// and cannot be spoofed through. That holds only while the origin is
    /// unreachable except through Cloudflare.
    #[default]
    Rightmost,
    /// The first entry. Only correct when every hop in the chain is trusted.
    Leftmost,
}

/// Where the client's address is read from.
///
/// Configured, never guessed. Trusting a forwarding header when nothing sets it
/// lets any caller pick their own bucket; trusting the socket when a proxy *is*
/// in front buckets the whole internet into one.
#[derive(Debug, Clone, Default)]
pub struct TrustedHeader {
    pub name: Option<String>,
    pub position: HeaderPosition,
}

impl TrustedHeader {
    /// The socket peer only — a bare process with nothing in front.
    pub fn peer() -> Self {
        Self::default()
    }
}

/// The client key for a request.
///
/// Falls back to the socket peer when the configured header is absent or
/// unusable, which is what a bare `cargo run` needs.
pub fn client_key(
    headers: &axum::http::HeaderMap,
    peer: SocketAddr,
    trusted: &TrustedHeader,
) -> ClientKey {
    let from_header = trusted.name.as_deref().and_then(|name| {
        let raw = headers.get(name)?.to_str().ok()?;
        let mut entries = raw.split(',').map(str::trim).filter(|s| !s.is_empty());
        match trusted.position {
            HeaderPosition::Leftmost => entries.next(),
            HeaderPosition::Rightmost => entries.next_back(),
        }
        .and_then(|s| s.parse::<IpAddr>().ok())
    });
    ClientKey::from_ip(from_header.unwrap_or_else(|| peer.ip()))
}

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
fn admission_units(len: usize) -> u32 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use governor::clock::FakeRelativeClock;

    fn quotas() -> ClientQuotas {
        ClientQuotas {
            units_per_second: 30,
            burst_units: 180,
            long_units: 3_000,
            long_window: Duration::from_secs(300),
        }
    }

    fn global_quotas() -> GlobalQuotas {
        GlobalQuotas {
            units_per_second: 300,
            burst_units: 1_500,
        }
    }

    fn key(n: u8) -> ClientKey {
        ClientKey::V4([10, 0, 0, n])
    }

    /// `quanta` — governor's default clock — does not follow tokio's paused
    /// clock, so these tests drive a fake clock explicitly rather than using
    /// `tokio::time::pause()`.
    fn limiter(q: ClientQuotas) -> (ClientLimiter<FakeRelativeClock>, FakeRelativeClock) {
        let clock = FakeRelativeClock::default();
        (ClientLimiter::new(q, clock.clone()).unwrap(), clock)
    }

    /// The two limiters as the service composes them: per-client first, so an
    /// abusive client is attributed to itself rather than showing up as the
    /// whole chain being saturated.
    fn peer() -> SocketAddr {
        "203.0.113.7:5000".parse().unwrap()
    }

    /// A wallet's boot burst is about 48 units. It must fit without touching
    /// the limit, or the app is rate-limited on first paint.
    #[test]
    fn a_wallets_boot_burst_is_well_within_budget() {
        let (l, _) = limiter(quotas());
        assert_eq!(l.check(key(1), 48), Decision::Allow);
        // And there is room for several more.
        for _ in 0..2 {
            assert_eq!(l.check(key(1), 48), Decision::Allow);
        }
    }

    /// The burst is finite, and exhausting it names the bucket that ran out.
    #[test]
    fn exhausting_the_short_burst_denies_with_a_retry_after() {
        let (l, _) = limiter(quotas());
        // 180 units of burst, spent 15 at a time.
        for _ in 0..12 {
            assert_eq!(l.check(key(2), 15), Decision::Allow);
        }
        match l.check(key(2), 15) {
            Decision::Deny {
                scope,
                retry_after_secs,
            } => {
                assert_eq!(scope, Scope::IpShort);
                assert!(retry_after_secs >= 1, "never tells a client to retry now");
            }
            other => panic!("burst should be exhausted, got {other:?}"),
        }
    }

    /// A request heavier than the whole burst cannot succeed however long the
    /// client waits, so it must not be told to wait. It is told the cost and the
    /// limit instead, and becomes a non-retryable error.
    #[test]
    fn a_request_larger_than_the_burst_is_never_admitted() {
        let (l, clock) = limiter(quotas());
        let d = l.check(key(9), 181);
        assert_eq!(
            d,
            Decision::Never {
                scope: Scope::IpShort,
                units: 181,
                capacity: 180,
            }
        );
        clock.advance(Duration::from_secs(3600));
        assert!(matches!(l.check(key(9), 181), Decision::Never { .. }));
        assert!(matches!(
            d.into_result(),
            Err(AppError::TooCostly {
                units: 181,
                limit: 180
            })
        ));
    }

    /// The bucket refills, so a denied client recovers without intervention.
    #[test]
    fn waiting_restores_the_budget() {
        let (l, clock) = limiter(quotas());
        for _ in 0..12 {
            l.check(key(3), 15);
        }
        assert!(matches!(l.check(key(3), 15), Decision::Deny { .. }));

        clock.advance(Duration::from_secs(1));
        // One second at 30 units/s buys two more 15-unit calls.
        assert_eq!(l.check(key(3), 15), Decision::Allow);
    }

    /// `Retry-After` must not overstate the wait, or a client sleeps longer
    /// than it needs to and the app feels broken.
    #[test]
    fn retry_after_is_a_real_wait_not_a_guess() {
        let (l, clock) = limiter(quotas());
        while matches!(l.check(key(4), 30), Decision::Allow) {}

        let Decision::Deny {
            retry_after_secs, ..
        } = l.check(key(4), 30)
        else {
            panic!("expected a denial")
        };

        clock.advance(Duration::from_secs(retry_after_secs));
        assert_eq!(
            l.check(key(4), 30),
            Decision::Allow,
            "waiting the advertised time must be enough"
        );
    }

    /// One abusive client must not spend another's budget.
    #[test]
    fn clients_have_separate_budgets() {
        let (l, _) = limiter(quotas());
        for _ in 0..12 {
            l.check(key(5), 15);
        }
        assert!(matches!(l.check(key(5), 15), Decision::Deny { .. }));
        assert_eq!(l.check(key(6), 15), Decision::Allow);
    }

    /// The credit guard. Per-IP limits cannot bound a distributed abuser, so
    /// the chain-wide bucket has to bite even when every client is individually
    /// within its own budget.
    #[test]
    fn the_global_bucket_bounds_a_distributed_flood() {
        let clock = FakeRelativeClock::default();
        let client = ClientLimiter::new(quotas(), clock.clone()).unwrap();
        let global = GlobalLimiter::new(global_quotas(), clock).unwrap();

        let mut denials = 0;
        // 200 distinct clients, each spending well under its own burst.
        for i in 0..200u8 {
            for _ in 0..2 {
                // The order the service uses: per-client, then chain-wide.
                let d = match client.check(ClientKey::V4([10, 1, 0, i]), 15) {
                    Decision::Allow => global.check(15),
                    deny => deny,
                };
                if let Decision::Deny { scope, .. } = d {
                    assert_eq!(scope, Scope::Global, "per-client budgets are untouched");
                    denials += 1;
                }
            }
        }
        assert!(denials > 0, "the chain-wide budget must eventually bite");
    }

    /// Weight is charged, not request count: one `eth_getLogs` costs fifteen
    /// times an `eth_blockNumber`, because it costs that much upstream.
    #[test]
    fn charging_is_weighted() {
        let (cheap, _) = limiter(quotas());
        let (dear, _) = limiter(quotas());

        let mut cheap_calls = 0;
        while matches!(cheap.check(key(7), 1), Decision::Allow) {
            cheap_calls += 1;
        }
        let mut dear_calls = 0;
        while matches!(dear.check(key(7), 15), Decision::Allow) {
            dear_calls += 1;
        }
        assert!(
            cheap_calls > dear_calls * 10,
            "{cheap_calls} cheap vs {dear_calls} expensive"
        );
    }

    /// `eth_chainId` is answered from config and reaches no upstream, so it
    /// must not consume budget.
    #[test]
    fn a_zero_weight_call_is_never_charged() {
        let (l, _) = limiter(quotas());
        for _ in 0..10_000 {
            assert_eq!(l.check(key(8), 0), Decision::Allow);
        }
        // And the budget it did not spend is still there.
        assert_eq!(l.check(key(8), 180), Decision::Allow);
    }

    /// A residential IPv6 client holds a whole /64, so per-address limiting is
    /// defeated for free.
    #[test]
    fn ipv6_is_bucketed_by_prefix_not_by_address() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff:ffff:ffff:ffff".parse().unwrap();
        let other: IpAddr = "2001:db8:1:3::1".parse().unwrap();

        assert_eq!(ClientKey::from_ip(a), ClientKey::from_ip(b));
        assert_ne!(ClientKey::from_ip(a), ClientKey::from_ip(other));
    }

    /// A v4-mapped v6 address is a v4 client. Bucketing it as a /64 would give
    /// every such client one shared key.
    #[test]
    fn a_v4_mapped_address_buckets_as_v4() {
        assert_eq!(
            ClientKey::from_ip("::ffff:203.0.113.7".parse().unwrap()),
            ClientKey::from_ip("203.0.113.7".parse().unwrap())
        );
    }

    fn trusted(name: &str, position: HeaderPosition) -> TrustedHeader {
        TrustedHeader {
            name: Some(name.into()),
            position,
        }
    }

    /// With no trusted header configured the socket peer is the only usable
    /// source. Honouring a header that nothing sets would let any caller choose
    /// its own bucket.
    #[test]
    fn an_unconfigured_header_is_ignored_even_when_present() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "198.51.100.9".parse().unwrap());

        assert_eq!(
            client_key(&h, peer(), &TrustedHeader::peer()),
            ClientKey::from_ip("203.0.113.7".parse().unwrap())
        );
    }

    /// The rate-limit bypass this defaults against. Every hop appends, so the
    /// left of `X-Forwarded-For` is whatever the caller wrote — and a caller
    /// who picks their own bucket can mint a fresh one per request.
    #[test]
    fn a_spoofed_prefix_does_not_choose_the_bucket() {
        let mut h = HeaderMap::new();
        // The first two entries are the caller's invention; the proxy appended
        // the third.
        h.insert(
            "x-forwarded-for",
            "1.2.3.4, 5.6.7.8, 198.51.100.9".parse().unwrap(),
        );

        assert_eq!(
            client_key(
                &h,
                peer(),
                &trusted("x-forwarded-for", HeaderPosition::Rightmost)
            ),
            ClientKey::from_ip("198.51.100.9".parse().unwrap()),
            "the entry the proxy appended is the only trustworthy one"
        );
        assert_eq!(
            client_key(
                &h,
                peer(),
                &trusted("x-forwarded-for", HeaderPosition::Leftmost)
            ),
            ClientKey::from_ip("1.2.3.4".parse().unwrap()),
            "leftmost is opt-in, and only safe when every hop is trusted"
        );
    }

    /// Rightmost is the default, so a config that omits the position is safe
    /// rather than exploitable.
    #[test]
    fn the_default_position_is_the_safe_one() {
        assert_eq!(HeaderPosition::default(), HeaderPosition::Rightmost);
        assert_eq!(TrustedHeader::default().position, HeaderPosition::Rightmost);
    }

    /// A configured header that is absent or unparseable falls back to the
    /// peer rather than bucketing every such request together.
    #[test]
    fn a_missing_or_malformed_header_falls_back_to_the_peer() {
        let t = trusted("cf-connecting-ip", HeaderPosition::Rightmost);
        let expected = ClientKey::from_ip("203.0.113.7".parse().unwrap());

        assert_eq!(client_key(&HeaderMap::new(), peer(), &t), expected);

        let mut h = HeaderMap::new();
        h.insert("cf-connecting-ip", "not-an-ip".parse().unwrap());
        assert_eq!(client_key(&h, peer(), &t), expected);

        // A trailing comma must not read as an empty final entry.
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "198.51.100.9, ".parse().unwrap());
        assert_eq!(
            client_key(
                &h,
                peer(),
                &trusted("x-forwarded-for", HeaderPosition::Rightmost)
            ),
            ClientKey::from_ip("198.51.100.9".parse().unwrap())
        );
    }

    /// The address must not reach a log, so the digest must not be reversible
    /// to it by inspection.
    #[test]
    fn the_log_digest_hides_the_address() {
        let d = ClientKey::from_ip("203.0.113.7".parse().unwrap()).digest();
        assert!(!d.contains("203"), "{d}");
        assert_eq!(d.len(), 12);
        // Stable, so one client's requests are still correlatable in a log.
        assert_eq!(
            d,
            ClientKey::from_ip("203.0.113.7".parse().unwrap()).digest()
        );
    }

    /// A burst below the sustained rate refuses one second of allowed traffic.
    /// Rejected at startup rather than surfacing as intermittent 429s.
    #[test]
    fn an_incoherent_quota_is_rejected_at_startup() {
        let mut q = quotas();
        q.burst_units = 10;
        assert!(ClientLimiter::new(q, FakeRelativeClock::default()).is_err());

        let mut q = quotas();
        q.units_per_second = 0;
        assert!(ClientLimiter::new(q, FakeRelativeClock::default()).is_err());

        let mut q = quotas();
        q.long_window = Duration::ZERO;
        assert!(ClientLimiter::new(q, FakeRelativeClock::default()).is_err());

        let mut g = global_quotas();
        g.burst_units = 10;
        assert!(GlobalLimiter::new(g, FakeRelativeClock::default()).is_err());
    }

    /// Parsing is the most expensive thing an unauthenticated caller can ask
    /// for, and it happens before any method weight is knowable. A flood of
    /// oversized bodies must therefore be bounded by the same budget real
    /// traffic spends, not answered for free.
    #[test]
    fn oversized_bodies_are_charged_before_they_are_parsed() {
        let (l, _) = limiter(quotas());

        // 256 KB is the body cap; 9 units apiece against a 180-unit burst.
        let mut accepted = 0;
        while l.admit(key(20), 256 * 1024).is_ok() {
            accepted += 1;
            assert!(accepted < 100, "the admission charge never bit");
        }
        assert!(
            accepted <= 20,
            "a 180-unit burst must not buy 20 oversized parses, got {accepted}"
        );
    }

    /// The floor matters as much as the slope: a tiny body still costs
    /// something, or a flood of empty POSTs is free.
    #[test]
    fn admission_has_a_floor_and_a_slope() {
        assert_eq!(admission_units(0), 1);
        assert_eq!(admission_units(1), 1);
        assert_eq!(admission_units(32 * 1024), 2);
        assert_eq!(admission_units(256 * 1024), 9);
    }

    /// An honest batch must not notice the admission charge.
    #[test]
    fn an_honest_body_is_charged_one_unit() {
        let (l, _) = limiter(quotas());
        // ~3 KB is a real twenty-call batch.
        assert!(l.admit(key(21), 3 * 1024).is_ok());
        // The boot burst still fits alongside it.
        assert_eq!(l.check(key(21), 48), Decision::Allow);
    }

    /// The leak this exists to stop. `governor` never retires an idle key on
    /// its own, so every distinct address seen since start would otherwise stay
    /// in the dashmap for the life of the process.
    #[test]
    fn idle_buckets_are_reclaimed() {
        let (l, clock) = limiter(quotas());

        for i in 0..200u8 {
            l.check(ClientKey::V4([10, 9, 0, i]), 1);
        }
        assert_eq!(
            l.key_counts()[0].1,
            200,
            "every distinct client should hold a bucket while it is active"
        );

        // Long enough that every bucket is back at full capacity, so retiring
        // it gives nothing away.
        clock.advance(Duration::from_secs(3_600));
        l.gc();

        assert_eq!(
            l.key_counts()[0].1,
            0,
            "idle buckets must not be held for the life of the process"
        );
        assert_eq!(l.key_counts()[1].1, 0);
    }

    /// A client mid-burst must keep its state across a GC pass, or the sweep
    /// would itself be the rate-limit bypass.
    #[test]
    fn gc_does_not_refund_an_active_client() {
        let (l, _) = limiter(quotas());
        for _ in 0..12 {
            l.check(key(22), 15);
        }
        assert!(matches!(l.check(key(22), 15), Decision::Deny { .. }));

        l.gc();

        assert!(
            matches!(l.check(key(22), 15), Decision::Deny { .. }),
            "a sweep must not hand back budget the client has already spent"
        );
    }

    /// The per-client buckets are one instance for the service, so a client
    /// reading three chains spends one budget rather than three.
    ///
    /// Built per chain, this would enforce three times what the config
    /// documents — and the config has only one `[rate_limit]` block to read.
    #[test]
    fn one_client_budget_is_shared_across_chains() {
        let clock = FakeRelativeClock::default();
        let client = ClientLimiter::new(quotas(), clock.clone()).unwrap();
        let chains: Vec<GlobalLimiter<FakeRelativeClock>> = (0..3)
            .map(|_| GlobalLimiter::new(global_quotas(), clock.clone()).unwrap())
            .collect();

        // Spend the whole per-client burst by way of chain 0.
        let mut spent = 0;
        while matches!(client.check(key(23), 15), Decision::Allow) {
            assert_eq!(chains[0].check(15), Decision::Allow);
            spent += 15;
        }

        assert!(spent <= 180, "spent {spent} against a 180-unit burst");
        // The other two chains are not a fresh allowance.
        assert!(
            matches!(client.check(key(23), 15), Decision::Deny { .. }),
            "a second chain must not reset the client's budget"
        );
    }
}
