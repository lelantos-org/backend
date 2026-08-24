//! Prometheus metrics: names, descriptions, and the exporter's listener.
//!
//! # Why the facade
//!
//! Instrumentation points are scattered across services that are built by
//! explicit DI (`ConsumeServiceImpl`, `AppState`, the tree mirror). Threading a
//! registry through every constructor for a cross-cutting concern is a large
//! diff for no gain, so this uses the global-recorder `metrics` facade instead.
//! With no recorder installed every macro is a no-op, which is what lets
//! `shared::tick::run` be instrumented once for every tick service while only
//! the binaries that call [`init`] emit anything.
//!
//! # Cardinality
//!
//! Every label value here is bounded: a handful of chains, a fixed set of
//! service names, a closed set of outcomes, and — for HTTP — the *matched route
//! template*, never the request path. `chunk_id` and `leaf_count` must never
//! become labels: one series per chunk would grow without limit and eventually
//! take out whatever scrapes this.

/// Metric names, in one place so the emitting site and any dashboard or test
/// referring to them cannot drift.
pub mod name {
    // fmd-webserver
    pub const HTTP_REQUESTS: &str = "http_requests_total";
    pub const HTTP_DURATION: &str = "http_request_duration_seconds";
    pub const TREE_MIRROR_LEAVES: &str = "tree_mirror_leaves";
    pub const TREE_MIRROR_REBUILDS: &str = "tree_mirror_rebuilds_total";
    pub const TREE_MIRROR_SYNC_DURATION: &str = "tree_mirror_sync_duration_seconds";
    pub const CACHE_REQUESTS: &str = "cache_requests_total";
    pub const TREE_STATE_REDIRECTS: &str = "tree_state_redirects_total";
    pub const TREE_STATE_BODIES: &str = "tree_state_bodies_total";
    pub const CHUNK_FEED_BYTES: &str = "chunk_feed_bytes_total";

    // fmd-indexer
    pub const TICK_DURATION: &str = "tick_duration_seconds";
    pub const TICK_PROGRESS: &str = "tick_progress_total";
    pub const TICK_ERRORS: &str = "tick_errors_total";
    pub const CONSUMER_CURSOR_EVENT_ID: &str = "consumer_cursor_event_id";
    pub const RAW_EVENTS_MAX_ID: &str = "raw_events_max_id";
    pub const REORGS_APPLIED: &str = "reorgs_applied_total";
    pub const NOTES_LEAF_INDEX_MAX: &str = "notes_leaf_index_max";
    pub const SPENT_NULLIFIERS_SEQ_MAX: &str = "spent_nullifiers_seq_max";
    pub const CHAIN_LEADER: &str = "chain_leader";
}

/// Label value for a request that matched no route.
///
/// Without this, 404s would label by raw path and every scanner probing the
/// origin would mint a permanent time series.
pub const UNMATCHED_ROUTE: &str = "<unmatched>";

/// Bucket boundaries for every duration histogram, in seconds.
///
/// One shared set rather than per-metric tuning: it has to span an HTTP request
/// that should take a millisecond and a cold tree rebuild that takes seconds,
/// and a single set keeps the series aggregatable across replicas. Explicit
/// buckets are the point — the exporter's default is per-process summary
/// quantiles, which cannot be summed at all.
#[cfg(feature = "metrics-exporter")]
const DURATION_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Install the Prometheus recorder and serve `/metrics` on `addr`.
///
/// The exporter owns the listener, which is why a service with no HTTP surface
/// of its own needs nothing else to be scrapable.
///
/// # On not enforcing loopback here
///
/// `/metrics` must not be publicly reachable, but that property cannot be
/// asserted at this bind. Under compose the process binds `0.0.0.0` inside its
/// own network namespace and the host publishes `127.0.0.1:<port>:<port>`;
/// binding container-loopback instead makes the published port unreachable,
/// because Docker forwards to the container's interface address rather than its
/// loopback. So a hard check here would refuse to start in the one deployment
/// that matters while proving nothing about host exposure.
///
/// The real guarantees live where they can be enforced: the loopback publish in
/// `compose.prod.yml.j2`, and the `deny_metrics` snippet in the Caddyfile. A
/// non-loopback bind is still worth saying out loud, so it warns.
///
/// Call once, from `main`, inside the tokio runtime.
#[cfg(feature = "metrics-exporter")]
pub fn init(addr: std::net::SocketAddr) -> anyhow::Result<()> {
    use metrics_exporter_prometheus::PrometheusBuilder;

    if !addr.ip().is_loopback() {
        tracing::warn!(
            %addr,
            "metrics listener is not bound to loopback; the host publish and the \
             reverse-proxy deny rule are the only things keeping /metrics private"
        );
    }

    PrometheusBuilder::new()
        .with_http_listener(addr)
        .set_buckets(DURATION_BUCKETS)?
        .install()?;

    describe();
    tracing::info!(%addr, "metrics listener installed");
    Ok(())
}

/// Units and help text. Purely descriptive, but it is what makes a scrape
/// readable without cross-referencing this file.
#[cfg(feature = "metrics-exporter")]
fn describe() {
    use ::metrics::{Unit, describe_counter, describe_gauge, describe_histogram};

    describe_counter!(name::HTTP_REQUESTS, Unit::Count, "HTTP responses served");
    describe_histogram!(
        name::HTTP_DURATION,
        Unit::Seconds,
        "HTTP request handling time"
    );
    describe_gauge!(
        name::TREE_MIRROR_LEAVES,
        Unit::Count,
        "leaves currently folded into the in-memory tree mirror"
    );
    describe_counter!(
        name::TREE_MIRROR_REBUILDS,
        Unit::Count,
        "tree mirror rebuilds from leaf 0, by what triggered them"
    );
    describe_histogram!(
        name::TREE_MIRROR_SYNC_DURATION,
        Unit::Seconds,
        "time spent bringing the tree mirror to the tip, including rebuilds"
    );
    describe_counter!(
        name::CACHE_REQUESTS,
        Unit::Count,
        "in-process cache lookups by cache and hit/miss"
    );
    describe_counter!(
        name::TREE_STATE_REDIRECTS,
        Unit::Count,
        "/v1/tree-state polls answered with a redirect to the content-addressed body"
    );
    describe_counter!(
        name::TREE_STATE_BODIES,
        Unit::Count,
        "content-addressed tree-state bodies served from origin"
    );
    describe_counter!(
        name::CHUNK_FEED_BYTES,
        Unit::Bytes,
        "uncompressed chunk-feed body bytes served"
    );

    describe_histogram!(name::TICK_DURATION, Unit::Seconds, "one tick of one chain");
    describe_counter!(
        name::TICK_PROGRESS,
        Unit::Count,
        "ticks by reported progress; a high idle share means the driver is caught up"
    );
    describe_counter!(
        name::TICK_ERRORS,
        Unit::Count,
        "ticks that returned an error"
    );
    describe_gauge!(
        name::CONSUMER_CURSOR_EVENT_ID,
        Unit::Count,
        "last raw_events id this consumer committed through"
    );
    describe_gauge!(
        name::RAW_EVENTS_MAX_ID,
        Unit::Count,
        "highest raw_events id ingested; minus the cursor, this is the lag"
    );
    describe_counter!(
        name::REORGS_APPLIED,
        Unit::Count,
        "reorg log entries retracted and replayed by this consumer"
    );
    describe_gauge!(
        name::NOTES_LEAF_INDEX_MAX,
        Unit::Count,
        "highest notes.leaf_index written"
    );
    describe_gauge!(
        name::SPENT_NULLIFIERS_SEQ_MAX,
        Unit::Count,
        "highest spent_nullifiers.seq written"
    );
    describe_gauge!(
        name::CHAIN_LEADER,
        Unit::Count,
        "1 when this replica holds the chain's advisory lock, else 0"
    );
}

/// HTTP middleware recording [`name::HTTP_REQUESTS`] and [`name::HTTP_DURATION`].
///
/// The `route` label is the **matched route template** from
/// [`axum::extract::MatchedPath`], so
/// `/v1/chains/:chain_id/commitments/chunks/:chunk_id` is one series rather
/// than one per chunk id. A request that matched nothing is labelled
/// [`UNMATCHED_ROUTE`] for the same reason.
#[cfg(feature = "webserver")]
pub async fn track_http(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::extract::MatchedPath;
    use std::time::Instant;

    // Cloned out of the extensions before the request is consumed. Owned
    // `String`s because the labels outlive the request either way.
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| UNMATCHED_ROUTE.to_owned());
    let method = req.method().as_str().to_owned();

    let start = Instant::now();
    let response = next.run(req).await;
    let elapsed = start.elapsed();

    ::metrics::histogram!(
        name::HTTP_DURATION,
        "route" => route.clone(),
        "method" => method.clone(),
    )
    .record(elapsed.as_secs_f64());
    ::metrics::counter!(
        name::HTTP_REQUESTS,
        "route" => route,
        "method" => method,
        "status" => response.status().as_u16().to_string(),
    )
    .increment(1);

    response
}

/// Record the serialised size of a chunk-feed response body.
///
/// Read from the body's exact size hint rather than re-serialising: `Json`
/// produces a single in-memory buffer, so the hint *is* the byte count and
/// costs nothing. `None` would mean a streaming body, which this feed never
/// produces — skipped rather than guessed, so the counter stays a true total.
///
/// This is the number that decides whether a binary wire format is ever worth
/// revisiting; PLAN.MD argued that case from estimates.
#[cfg(feature = "webserver")]
pub fn record_chunk_feed_bytes(kind: &'static str, resp: &axum::response::Response) {
    use axum::body::HttpBody;

    if let Some(bytes) = resp.body().size_hint().exact() {
        ::metrics::counter!(name::CHUNK_FEED_BYTES, "kind" => kind).increment(bytes);
    }
}

/// Record a cache lookup outcome.
///
/// `cache` names the field in the app's cache struct; `outcome` is `"hit"` or
/// `"miss"`. Both label sets are closed, so this cannot grow series.
#[cfg(feature = "webserver")]
pub fn record_cache(cache: &'static str, hit: bool) {
    ::metrics::counter!(
        name::CACHE_REQUESTS,
        "cache" => cache,
        "outcome" => if hit { "hit" } else { "miss" },
    )
    .increment(1);
}

/// Hit/miss for a `moka` `try_get_with`, which reports neither itself.
///
/// `moka` runs the initialiser only on a miss, so a flag set inside it is the
/// only signal available. The flag lives behind an `Arc` because the
/// initialiser is an `async move` block and cannot borrow the caller's frame.
///
/// ```ignore
/// let probe = CacheProbe::new("notes_pages");
/// let miss = probe.marker();
/// let out = cache.try_get_with(key, async move { miss.mark(); load().await }).await;
/// probe.record();
/// ```
#[cfg(feature = "webserver")]
pub struct CacheProbe {
    cache: &'static str,
    missed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Cheap clonable handle that marks its [`CacheProbe`] as a miss.
#[cfg(feature = "webserver")]
#[derive(Clone)]
pub struct MissMarker(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(feature = "webserver")]
impl MissMarker {
    pub fn mark(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(feature = "webserver")]
impl CacheProbe {
    pub fn new(cache: &'static str) -> Self {
        Self {
            cache,
            missed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Hand this to the `try_get_with` initialiser.
    pub fn marker(&self) -> MissMarker {
        MissMarker(self.missed.clone())
    }

    /// Emit the outcome. Call once, after `try_get_with` has returned —
    /// including on the error path, where a failed load is still a miss.
    pub fn record(&self) {
        let missed = self.missed.load(std::sync::atomic::Ordering::Relaxed);
        record_cache(self.cache, !missed);
    }
}

#[cfg(all(test, feature = "webserver"))]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use metrics::{Counter, Gauge, Histogram};
    use metrics::{Key, KeyName, Metadata, Recorder, SharedString, Unit};
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    /// Records the full key (name + labels) of every counter touched.
    ///
    /// The assertion this exists for is about *series count*, so the recorder
    /// has to keep keys rather than values.
    #[derive(Clone, Default)]
    struct KeyCapture(Arc<Mutex<HashSet<String>>>);

    impl Recorder for KeyCapture {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
            self.0.lock().unwrap().insert(key.to_string());
            Counter::noop()
        }
        fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
            Gauge::noop()
        }
        fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
            Histogram::noop()
        }
    }

    fn app() -> Router {
        Router::new()
            .route(
                "/v1/chains/:chain_id/commitments/chunks/:chunk_id",
                get(|| async { "ok" }),
            )
            .layer(axum::middleware::from_fn(track_http))
    }

    async fn hit(app: &Router, uri: &str) -> StatusCode {
        app.clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    /// The cardinality guard.
    ///
    /// Labelling by request path instead of matched route would mint one time
    /// series per chunk id — unbounded, and eventually fatal to whatever
    /// scrapes this. Distinct chunk ids must collapse onto a single series, and
    /// a path that matched no route must not create one of its own.
    #[tokio::test]
    async fn distinct_chunk_ids_share_one_series() {
        let capture = KeyCapture::default();
        // `with_local_recorder` keeps this off the process-global recorder, so
        // the test cannot be perturbed by another test installing one.
        let keys = metrics::with_local_recorder(&capture, || {
            futures::executor::block_on(async {
                let app = app();
                for chunk_id in [0, 1, 2, 4095] {
                    let uri = format!("/v1/chains/1/commitments/chunks/{chunk_id}");
                    assert_eq!(hit(&app, &uri).await, StatusCode::OK);
                }
                assert_eq!(hit(&app, "/nope/deadbeef").await, StatusCode::NOT_FOUND);
                assert_eq!(hit(&app, "/nope/cafebabe").await, StatusCode::NOT_FOUND);
            });
            capture.0.lock().unwrap().clone()
        });

        let matched: Vec<_> = keys.iter().filter(|k| k.contains("commitments")).collect();
        assert_eq!(
            matched.len(),
            1,
            "four chunk ids must share one series, got {matched:?}"
        );
        assert!(
            matched[0].contains(":chunk_id"),
            "route label must be the template, got {matched:?}"
        );

        let unmatched: Vec<_> = keys
            .iter()
            .filter(|k| k.contains(UNMATCHED_ROUTE))
            .collect();
        assert_eq!(
            unmatched.len(),
            1,
            "two unmatched paths must share one series, got {unmatched:?}"
        );
        assert!(
            !keys
                .iter()
                .any(|k| k.contains("deadbeef") || k.contains("cafebabe")),
            "a raw path leaked into a label: {keys:?}"
        );
    }
}
