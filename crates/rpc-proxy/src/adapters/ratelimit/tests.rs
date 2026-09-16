use super::limiter::admission_units;
use super::*;
use axum::http::HeaderMap;
use governor::clock::FakeRelativeClock;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

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
