//! Prometheus metrics: names, descriptions, and the exporter's listener.
//!
//! # Facade
//!
//! Instrumentation points are spread across services built by explicit DI
//! (`ConsumeServiceImpl`, `AppState`, the tree mirror), so this uses the
//! global-recorder `metrics` facade rather than threading a registry through
//! every constructor. With no recorder installed every macro is a no-op, which
//! lets `shared::tick::run` be instrumented once for every tick service while
//! only the binaries that call [`init`] emit anything.
//!
//! # Cardinality
//!
//! Every label value is bounded: a handful of chains, a fixed set of service
//! names, a closed set of outcomes, and for HTTP the matched route template
//! rather than the request path. `chunk_id` and `leaf_count` must never become
//! labels; one series per chunk would grow without limit.

/// Metric names, kept in one place so emitting sites, dashboards and tests
/// cannot drift apart.
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
    pub const NOTES_MAX_ID: &str = "notes_max_id";
    pub const CONSUMER_CURSOR_NOTE_ID: &str = "consumer_cursor_note_id";

    // ingester
    pub const INGESTER_STAGE_DURATION: &str = "ingester_stage_seconds";
    pub const INGESTER_RPC_CALLS: &str = "ingester_rpc_calls_total";
    pub const INGESTER_RPC_ERRORS: &str = "ingester_rpc_errors_total";
    pub const INGESTER_CHAIN_LAG: &str = "ingester_chain_lag_blocks";
    pub const INGESTER_RETRIES: &str = "ingester_retries_total";

    // Cross-service. Emitted by every stage that commits derived state; the
    // `stage` label makes one pipeline's latency readable end to end.
    pub const EVENT_AGE: &str = "event_age_seconds";
}

/// Pipeline stages that report [`name::EVENT_AGE`].
///
/// Named rather than written inline at each call site: the `stage` label set
/// must stay closed, and a typo in a bare literal would mint an unwatched time
/// series instead of failing.
pub mod stage {
    /// Chain -> `raw_events`, recorded by the ingester's live tail.
    pub const INGEST: &str = "ingest";
    /// `raw_events` -> `notes`, recorded by fmd-indexer's consume loop.
    pub const CONSUME: &str = "consume";
}

/// Steps of one ingester tick or backfill chunk, reported by
/// [`name::INGESTER_STAGE_DURATION`].
///
/// A separate closed set from [`stage`]: that one names whole pipeline hops for
/// [`name::EVENT_AGE`], these name the round trips inside a single hop. Sharing
/// one module would let an `EVENT_AGE` call site pass `GET_LOGS` and mint a
/// series no dashboard reads.
pub mod ingest_stage {
    /// Asking the chain for the anchor's hash and the tip.
    pub const ANCHOR: &str = "anchor";
    /// Reading the cursor the tick plans from.
    pub const PLAN: &str = "plan";
    /// `eth_getLogs`, including the adaptive window's shrink/grow search.
    pub const GET_LOGS: &str = "get_logs";
    /// Resolving per-block metadata for the blocks the logs touched.
    pub const BLOCK_META: &str = "block_meta";
    /// The insert-and-advance transaction, including the `pg_notify` it carries.
    pub const COMMIT: &str = "commit";
}

/// Label value for a request that matched no route.
///
/// Without it, 404s would label by raw path and every scanner probing the
/// origin would mint a permanent time series.
pub const UNMATCHED_ROUTE: &str = "<unmatched>";

/// Bucket boundaries for every duration histogram, in seconds.
///
/// One shared set rather than per-metric tuning: it must span a millisecond
/// HTTP request and a multi-second cold tree rebuild, and a single set keeps the
/// series aggregatable across replicas. Explicit buckets are required because
/// the exporter defaults to per-process summary quantiles, which cannot be
/// summed.
#[cfg(feature = "metrics-exporter")]
const DURATION_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Install the Prometheus recorder and serve `/metrics` on `addr`.
///
/// The exporter owns the listener, so a service with no HTTP surface of its own
/// is scrapable without further wiring.
///
/// # Loopback is not enforced here
///
/// `/metrics` must not be publicly reachable, but that cannot be asserted at
/// this bind. Under compose the process binds `0.0.0.0` inside its own network
/// namespace while the host publishes `127.0.0.1:<port>:<port>`; binding
/// container-loopback would make the published port unreachable, since Docker
/// forwards to the container's interface address. A hard check here would
/// therefore break the production deployment while proving nothing about host
/// exposure.
///
/// The guarantees are enforced by the loopback publish in `compose.prod.yml.j2`
/// and the `deny_metrics` snippet in the Caddyfile. A non-loopback bind still
/// warns.
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

/// [`init`], with the address still in its configured string form.
///
/// Every binary that serves `/metrics` reads the address from config, so the
/// parse and its error context belong here rather than in each `main`.
#[cfg(feature = "metrics-exporter")]
pub fn init_addr(addr: &str) -> anyhow::Result<()> {
    use anyhow::Context;

    let parsed = addr
        .parse()
        .with_context(|| format!("metrics_addr {addr}"))?;
    init(parsed).context("install metrics listener")
}

/// Units and help text, so a scrape is readable without cross-referencing this
/// file.
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
    describe_gauge!(
        name::NOTES_MAX_ID,
        Unit::Count,
        "highest notes id written; minus the cursor, this is the filter's lag"
    );
    describe_gauge!(
        name::CONSUMER_CURSOR_NOTE_ID,
        Unit::Count,
        "last notes id the filter scanned through"
    );

    describe_histogram!(
        name::INGESTER_STAGE_DURATION,
        Unit::Seconds,
        "one round trip inside an ingester tick or backfill chunk, by stage"
    );
    describe_counter!(
        name::INGESTER_RPC_CALLS,
        Unit::Count,
        "JSON-RPC calls issued, by method; divided by blocks ingested this is the \
         per-block provider cost"
    );
    describe_counter!(
        name::INGESTER_RPC_ERRORS,
        Unit::Count,
        "JSON-RPC failures by the class the ingester acts on: a range cap narrows \
         the window, a rate limit backs off"
    );
    describe_gauge!(
        name::INGESTER_CHAIN_LAG,
        Unit::Count,
        "blocks between the chain tip and last_scanned_block"
    );
    describe_counter!(
        name::INGESTER_RETRIES,
        Unit::Count,
        "retried operations, by what was being retried"
    );

    describe_histogram!(
        name::EVENT_AGE,
        Unit::Seconds,
        "wall-clock age of the freshest event a stage just committed, from its \
         block timestamp; the end-to-end latency signal"
    );
}

/// Record how old the freshest event in a just-committed batch is.
///
/// Called once per commit rather than once per row: the newest event bounds how
/// stale a reader can be, and per-row samples would let a wide backfill batch
/// dominate the histogram.
///
/// `block_ts` is the chain's own second-resolution timestamp, so a host clock
/// behind the node yields a negative age. Clamped at zero rather than skipped,
/// so a skewed host reads as instant instead of vanishing from the histogram.
pub fn record_event_age(stage: &'static str, chain_id: i64, block_ts: i64) {
    let age = chrono::Utc::now().timestamp() - block_ts;
    ::metrics::histogram!(
        name::EVENT_AGE,
        "stage" => stage,
        "chain_id" => chain_id.to_string(),
    )
    .record(age.max(0) as f64);
}

/// Time one step of an ingester tick and record it under [`name::INGESTER_STAGE_DURATION`].
///
/// Wraps the future rather than exposing a start/stop pair so a stage cannot be
/// started and never recorded, and so an early `?` inside the awaited operation
/// still reports the time it consumed: a stage that fails slowly is exactly what
/// the histogram is for.
///
/// `stage` must come from [`ingest_stage`].
pub async fn timed_ingest_stage<T>(
    stage: &'static str,
    chain_id: i64,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let started = std::time::Instant::now();
    let out = fut.await;
    ::metrics::histogram!(
        name::INGESTER_STAGE_DURATION,
        "stage" => stage,
        "chain_id" => chain_id.to_string(),
    )
    .record(started.elapsed().as_secs_f64());
    out
}

/// Count one outbound JSON-RPC call.
///
/// `method` is the wire method name. Closed by construction: the ingester's RPC
/// adapter calls a fixed handful of methods, so this never carries provider text.
pub fn record_rpc_call(method: &'static str, chain_id: i64) {
    ::metrics::counter!(
        name::INGESTER_RPC_CALLS,
        "method" => method,
        "chain_id" => chain_id.to_string(),
    )
    .increment(1);
}

/// Count one JSON-RPC failure by the class the caller acts on.
///
/// `class` is the discriminant of the error taxonomy, never the provider's
/// message: those are unbounded free text and would mint a series per wording.
pub fn record_rpc_error(class: &'static str, chain_id: i64) {
    ::metrics::counter!(
        name::INGESTER_RPC_ERRORS,
        "class" => class,
        "chain_id" => chain_id.to_string(),
    )
    .increment(1);
}

/// Report how far behind the chain tip this chain's cursor is.
pub fn record_chain_lag(chain_id: i64, lag: i64) {
    ::metrics::gauge!(
        name::INGESTER_CHAIN_LAG,
        "chain_id" => chain_id.to_string(),
    )
    .set(lag as f64);
}

/// Count one retried operation.
///
/// `what` names the operation being retried, matching the string the retry layer
/// already logs, so a spike in the counter and the corresponding warnings line up.
pub fn record_retry(what: &'static str, chain_id: i64) {
    ::metrics::counter!(
        name::INGESTER_RETRIES,
        "what" => what,
        "chain_id" => chain_id.to_string(),
    )
    .increment(1);
}

/// Report whether this replica currently holds `chain_id`'s advisory lock.
///
/// Gauged on both branches: failover would otherwise show up only as a replica
/// going quiet, which is indistinguishable from a stall.
pub fn record_chain_leader(chain_id: i64, leader: bool) {
    ::metrics::gauge!(
        name::CHAIN_LEADER,
        "chain_id" => chain_id.to_string(),
    )
    .set(if leader { 1.0 } else { 0.0 });
}

/// HTTP middleware recording [`name::HTTP_REQUESTS`] and [`name::HTTP_DURATION`].
///
/// The `route` label is the matched route template from
/// [`axum::extract::MatchedPath`], so
/// `/v1/chains/{chain_id}/commitments/chunks/{chunk_id}` is one series rather than
/// one per chunk id. A request that matched nothing is labelled
/// [`UNMATCHED_ROUTE`] for the same reason.
#[cfg(feature = "webserver")]
pub async fn track_http(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::extract::MatchedPath;
    use std::time::Instant;

    // Cloned out of the extensions before the request is consumed; the labels
    // outlive the request, so owned `String`s are required.
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
/// produces a single in-memory buffer, so the hint is the byte count. `None`
/// would mean a streaming body, which this feed never produces, and is skipped
/// rather than estimated so the counter stays an exact total.
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
/// `"miss"`. Both label sets are closed, so series count is bounded.
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
/// only available signal. The flag lives behind an `Arc` because the initialiser
/// is an `async move` block and cannot borrow the caller's frame.
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

    /// Emit the outcome. Call once after `try_get_with` returns, including on
    /// the error path, where a failed load still counts as a miss.
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
    /// The assertion is about series count, so the recorder keeps keys rather
    /// than values.
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
                "/v1/chains/{chain_id}/commitments/chunks/{chunk_id}",
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
    /// Labelling by request path instead of matched route would mint one
    /// unbounded time series per chunk id. Distinct chunk ids must collapse onto
    /// a single series, and a path that matched no route must not create one.
    #[tokio::test]
    async fn distinct_chunk_ids_share_one_series() {
        let capture = KeyCapture::default();
        // `with_local_recorder` keeps this off the process-global recorder, so
        // another test installing one cannot perturb this.
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
            matched[0].contains("{chunk_id}"),
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
