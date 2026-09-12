//! End-to-end behaviour, driven through the real router against a counting
//! fake upstream.
//!
//! Crate-local rather than in `backend/crates/integration-tests`, which is
//! Postgres-backed; this service has no database.
//!
//! The fake upstream is hand-written because these assertions are about how
//! many calls reached it and what they contained.

use alloy::primitives::{Address, address};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use rpc_proxy::adapters::ratelimit::{
    ClientLimiter, ClientQuotas, GlobalLimiter, GlobalQuotas, TrustedHeader,
};
use rpc_proxy::adapters::upstream::{Answer, OutboundCall, Upstream};
use rpc_proxy::app::AppState;
use rpc_proxy::domain::error::AppResult;
use rpc_proxy::domain::targets::Targets;
use rpc_proxy::services::proxy::{ChainLimits, ChainService};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const CHAIN: u64 = 31337;
const MASP: Address = address!("1111111111111111111111111111111111111111");
const PERMIT2: Address = address!("2222222222222222222222222222222222222222");
const TOKEN: Address = address!("3333333333333333333333333333333333333333");
/// `balanceOf(address)`.
const BALANCE_OF: &str = "0x70a08231";

/// An upstream that answers from a script and counts what it was asked.
struct FakeUpstream {
    /// Answers by method. A method may be scripted more than once; the last
    /// entry repeats.
    script: Mutex<HashMap<String, Vec<Scripted>>>,
    /// Every batch that arrived, as its list of methods.
    batches: Mutex<Vec<Vec<String>>>,
    calls: AtomicUsize,
    /// Delay before answering, so concurrent callers genuinely overlap.
    delay: std::time::Duration,
}

impl FakeUpstream {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(HashMap::new()),
            batches: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            delay: std::time::Duration::ZERO,
        })
    }

    fn with_delay(delay: std::time::Duration) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(HashMap::new()),
            batches: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            delay,
        })
    }

    /// Queue a result for `method`.
    fn answer(self: &Arc<Self>, method: &str, value: Value) -> Arc<Self> {
        self.push(method, Scripted::Result(value))
    }

    /// Queue a JSON-RPC error object as the answer for `method`.
    fn fail(self: &Arc<Self>, method: &str, error: Value) -> Arc<Self> {
        self.push(method, Scripted::Error(error))
    }

    fn push(self: &Arc<Self>, method: &str, answer: Scripted) -> Arc<Self> {
        self.script
            .lock()
            .unwrap()
            .entry(method.into())
            .or_default()
            .push(answer);
        Arc::clone(self)
    }

    /// How many times the upstream was called, batches counted once.
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn batches(&self) -> Vec<Vec<String>> {
        self.batches.lock().unwrap().clone()
    }

    fn next(&self, method: &str) -> Scripted {
        let mut s = self.script.lock().unwrap();
        let q = s.get_mut(method).unwrap_or_else(|| {
            panic!("no scripted answer for {method}");
        });
        if q.len() > 1 {
            q.remove(0)
        } else {
            q[0].clone()
        }
    }
}

#[async_trait]
impl Upstream for FakeUpstream {
    fn permits_available(&self) -> usize {
        usize::MAX
    }

    async fn call(
        &self,
        calls: &[OutboundCall<'_>],
        _needs_archive: bool,
    ) -> AppResult<Vec<Answer>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.batches
            .lock()
            .unwrap()
            .push(calls.iter().map(|c| c.method.to_string()).collect());
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(calls
            .iter()
            .map(|c| match self.next(c.method) {
                Scripted::Result(v) => {
                    Answer::Result(RawValue::from_string(v.to_string()).unwrap())
                }
                Scripted::Error(e) => Answer::Error(e),
            })
            .collect())
    }
}

/// One queued answer.
#[derive(Clone)]
enum Scripted {
    Result(Value),
    Error(Value),
}

/// Both quota sets. They are separate types in the service because the
/// per-client buckets are shared across chains while the credit guard is per
/// chain; the tests almost always want to set them together.
#[derive(Clone, Copy)]
struct Quotas {
    client: ClientQuotas,
    global: GlobalQuotas,
}

fn generous() -> Quotas {
    Quotas {
        client: ClientQuotas {
            units_per_second: 10_000,
            burst_units: 100_000,
            long_units: 1_000_000,
            long_window: std::time::Duration::from_secs(300),
        },
        global: GlobalQuotas {
            units_per_second: 10_000,
            burst_units: 100_000,
        },
    }
}

/// The router, over one chain served by `up`.
///
/// The client limiter is built here and handed to both the state and the chain
/// service: it must be the same instance, since the handler charges admission
/// against it before the parse and the service charges method weights against
/// it afterwards. Two instances would give every request two budgets.
fn app(up: Arc<dyn Upstream>, quotas: Quotas) -> Router {
    let client_limiter = Arc::new(ClientLimiter::new(quotas.client, Default::default()).unwrap());
    let chain = ChainService::new(
        CHAIN,
        MASP,
        Targets::new(MASP, PERMIT2, [TOKEN], []),
        ChainLimits {
            max_log_range: 5_000,
            max_call_data_bytes: 8 * 1024,
            max_call_gas: 50_000_000,
            reorg_depth: 64,
            deploy_block: None,
        },
        up,
        Arc::clone(&client_limiter),
        GlobalLimiter::new(quotas.global, Default::default()).unwrap(),
    )
    .unwrap();

    rpc_proxy::build_router(AppState {
        chains: Arc::new(HashMap::from([(CHAIN, Arc::new(chain))])),
        max_batch: 100,
        trusted_header: Arc::new(TrustedHeader::peer()),
        client_limiter,
    })
}

/// Dispatch without binding a port. `ConnectInfo` is inserted by hand because
/// `oneshot` bypasses the connect-info service the binary uses.
async fn post(app: &Router, body: Value) -> (StatusCode, Value, Vec<(String, String)>) {
    post_raw(app, body.to_string()).await
}

/// [`post`] with a body that need not be valid JSON.
async fn post_raw(
    app: &Router,
    body: impl Into<String>,
) -> (StatusCode, Value, Vec<(String, String)>) {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/v1/{CHAIN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.into()))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "203.0.113.7:5000".parse::<std::net::SocketAddr>().unwrap(),
    ));

    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, headers)
}

fn call(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// The headline. A hundred wallets polling the same block number must cost one
/// upstream call, not a hundred — this is the single largest saving the proxy
/// makes, and it is what the in-flight map is here for.
#[tokio::test]
async fn concurrent_identical_reads_collapse_to_one_upstream_call() {
    let up = FakeUpstream::with_delay(std::time::Duration::from_millis(50));
    up.answer("eth_blockNumber", json!("0xf4240"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..50 {
        let app = app.clone();
        set.spawn(async move { post(&app, call(1, "eth_blockNumber", json!([]))).await.1 });
    }

    let mut answers = Vec::new();
    while let Some(r) = set.join_next().await {
        answers.push(r.unwrap());
    }

    assert_eq!(
        up.calls(),
        1,
        "50 concurrent identical reads, one upstream call"
    );
    assert_eq!(answers.len(), 50);
    for a in answers {
        assert_eq!(a["result"], "0xf4240", "every caller got the answer");
    }
}

/// A wallet's poll as viem batches it: the head alongside one of its own reads.
fn head_and_balance(addr: &str) -> Value {
    json!([
        call(1, "eth_blockNumber", json!([])),
        call(2, "eth_getBalance", json!([addr, "latest"])),
    ])
}

/// The same herd, arriving the way viem actually sends it: each wallet's head
/// poll batched alongside its own reads. Every batch has two misses, so a
/// coalescing path reserved for lone calls would re-fetch the head once per
/// wallet.
#[tokio::test]
async fn overlapping_batches_fetch_a_shared_key_once() {
    let up = FakeUpstream::with_delay(std::time::Duration::from_millis(50));
    up.answer("eth_blockNumber", json!("0xf4240"));
    up.answer("eth_getBalance", json!("0x1"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let mut set = tokio::task::JoinSet::new();
    for i in 0..20u64 {
        let app = app.clone();
        // A distinct address per wallet, so only the head is shared.
        let addr = format!("0x{i:040x}");
        set.spawn(async move { post(&app, head_and_balance(&addr)).await.1 });
    }
    while let Some(r) = set.join_next().await {
        let body = r.unwrap();
        assert_eq!(body[0]["result"], "0xf4240", "{body}");
        assert_eq!(body[1]["result"], "0x1", "{body}");
    }

    let head_fetches = up
        .batches()
        .iter()
        .filter(|b| b.contains(&"eth_blockNumber".to_string()))
        .count();
    assert_eq!(head_fetches, 1, "20 overlapping batches, one head fetch");
    assert_eq!(
        up.batches()
            .iter()
            .map(|b| b.iter().filter(|m| *m == "eth_getBalance").count())
            .sum::<usize>(),
        20,
        "every wallet's own read still went upstream"
    );
}

/// A lone call must be able to join a key a batch is already fetching, not
/// only the other way round.
#[tokio::test]
async fn a_lone_call_joins_a_key_a_batch_is_fetching() {
    let up = FakeUpstream::with_delay(std::time::Duration::from_millis(100));
    up.answer("eth_blockNumber", json!("0xf4240"));
    up.answer("eth_getBalance", json!("0x1"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let batch = {
        let app = app.clone();
        let addr = format!("0x{}", "cd".repeat(20));
        tokio::spawn(async move { post(&app, head_and_balance(&addr)).await })
    };
    // Well inside the batch's upstream delay.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let (_, lone, _) = post(&app, call(3, "eth_blockNumber", json!([]))).await;
    batch.await.unwrap();

    assert_eq!(lone["result"], "0xf4240");
    assert_eq!(up.calls(), 1, "the lone call waited on the batch");
}

/// A second read inside the TTL is free; once it expires the upstream is asked
/// again, so the cache cannot pin a stale head.
#[tokio::test]
async fn a_cached_answer_expires() {
    let up = FakeUpstream::new();
    up.answer("eth_blockNumber", json!("0xf4240"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    post(&app, call(1, "eth_blockNumber", json!([]))).await;
    post(&app, call(2, "eth_blockNumber", json!([]))).await;
    assert_eq!(up.calls(), 1, "the second read is served from cache");

    // The head class is a one-second TTL.
    tokio::time::sleep(std::time::Duration::from_millis(1_300)).await;
    post(&app, call(3, "eth_blockNumber", json!([]))).await;
    assert_eq!(up.calls(), 2, "an expired entry is refetched");
}

/// The deposit-confirmation case. An unmined receipt is `null`, and caching it
/// would make every user watch the spinner for the TTL after their transaction
/// actually lands.
#[tokio::test]
async fn an_unmined_receipt_is_never_cached_but_a_mined_one_is() {
    let hash = format!("0x{}", "ab".repeat(32));
    let up = FakeUpstream::new();
    // First two polls find nothing; the third finds a deeply-mined receipt.
    up.answer("eth_getTransactionReceipt", json!(null));
    up.answer("eth_getTransactionReceipt", json!(null));
    up.answer("eth_getTransactionReceipt", json!({"blockNumber": "0x64"}));
    // A head far above the receipt's block, so it counts as final.
    up.answer("eth_blockNumber", json!("0xf4240"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    // Establish the head first, so the receipt can be classified as final.
    post(&app, call(0, "eth_blockNumber", json!([]))).await;
    let before = up.calls();

    for id in 1..=2 {
        let (_, body, _) = post(&app, call(id, "eth_getTransactionReceipt", json!([hash]))).await;
        assert_eq!(body["result"], Value::Null);
    }
    assert_eq!(
        up.calls() - before,
        2,
        "a pending receipt is re-asked every poll, never cached"
    );

    let (_, body, _) = post(&app, call(3, "eth_getTransactionReceipt", json!([hash]))).await;
    assert_eq!(body["result"]["blockNumber"], "0x64");
    let after_found = up.calls();

    post(&app, call(4, "eth_getTransactionReceipt", json!([hash]))).await;
    assert_eq!(
        up.calls(),
        after_found,
        "once found, a deeply-mined receipt is served from cache"
    );
}

/// The permit2 burst. A batch with some entries cached must forward only the
/// misses, as one batch, and come back in request order with the client's own
/// ids.
#[tokio::test]
async fn a_batch_forwards_only_its_misses_and_preserves_order() {
    let up = FakeUpstream::new();
    up.answer("eth_chainId", json!("0x7a69"));
    up.answer("eth_blockNumber", json!("0xf4240"));
    up.answer("eth_getBalance", json!("0x1"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    // Warm the block number so one of the three is a hit.
    post(&app, call(0, "eth_blockNumber", json!([]))).await;
    let before = up.calls();

    let addr = format!("0x{}", "cd".repeat(20));
    let (status, body, _) = post(
        &app,
        json!([
            call(101, "eth_blockNumber", json!([])),
            call(102, "eth_getBalance", json!([addr, "latest"])),
            // Answered from config; never reaches the upstream at all.
            call(103, "eth_chainId", json!([])),
        ]),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("a batch answers with an array");
    assert_eq!(arr.len(), 3);
    assert_eq!(
        arr.iter()
            .map(|r| r["id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![101, 102, 103],
        "responses come back in request order with the client's ids"
    );
    assert_eq!(arr[0]["result"], "0xf4240");
    assert_eq!(arr[1]["result"], "0x1");
    assert_eq!(arr[2]["result"], format!("0x{CHAIN:x}"));

    assert_eq!(up.calls() - before, 1, "one upstream round trip");
    assert_eq!(
        up.batches().last().unwrap(),
        &vec!["eth_getBalance".to_string()],
        "only the miss is forwarded"
    );
}

/// A forbidden method must fail fast and legibly, and must cost nothing
/// upstream. HTTP 200 is deliberate: viem does not retry `-32601`, whereas an
/// HTTP 4xx becomes an opaque transport error it retries three times first.
#[tokio::test]
async fn a_forbidden_method_is_refused_without_touching_the_upstream() {
    let up = FakeUpstream::new();
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    for method in [
        "eth_sendRawTransaction",
        "eth_estimateGas",
        "eth_getTransactionCount",
        "eth_feeHistory",
        "eth_subscribe",
    ] {
        let (status, body, _) = post(&app, call(1, method, json!([]))).await;
        assert_eq!(status, StatusCode::OK, "{method}");
        assert_eq!(body["error"]["code"], -32601, "{method}");
        assert!(
            body["error"]["message"].as_str().unwrap().contains(method),
            "the refusal names the method: {body}"
        );
    }
    assert_eq!(up.calls(), 0, "nothing reached the upstream");
}

/// An `eth_call` to a contract outside the allowlist is refused before any
/// upstream call. This is what stops the endpoint being a general-purpose read
/// node billed to our account.
#[tokio::test]
async fn an_unlisted_contract_is_refused_without_touching_the_upstream() {
    let up = FakeUpstream::new();
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let stranger = "0x9999999999999999999999999999999999999999";
    let (status, body, _) = post(
        &app,
        call(
            1,
            "eth_call",
            json!([{"to": stranger, "data": BALANCE_OF}, "latest"]),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32602);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not served"),
        "{body}"
    );
    assert_eq!(up.calls(), 0);

    // The allowlisted token on the same selector is served.
    up.answer("eth_call", json!("0x2a"));
    let (_, body, _) = post(
        &app,
        call(
            2,
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": BALANCE_OF}, "latest"]),
        ),
    )
    .await;
    assert_eq!(body["result"], "0x2a");
}

/// Over budget must be an HTTP 429 with a usable `Retry-After`: that is the one
/// shape viem recovers from on its own, without any SDK change.
#[tokio::test]
async fn exceeding_the_budget_returns_429_with_retry_after() {
    let up = FakeUpstream::new();
    up.answer("eth_blockNumber", json!("0xf4240"));
    let tight = Quotas {
        client: ClientQuotas {
            units_per_second: 1,
            burst_units: 3,
            long_units: 10,
            long_window: std::time::Duration::from_secs(300),
        },
        global: GlobalQuotas {
            units_per_second: 1_000,
            burst_units: 1_000,
        },
    };
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, tight);

    let mut limited = None;
    for id in 0..20 {
        let (status, _, headers) = post(&app, call(id, "eth_blockNumber", json!([]))).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(headers);
            break;
        }
    }

    let headers = limited.expect("the budget must run out");
    let retry = headers
        .iter()
        .find(|(k, _)| k == "retry-after")
        .expect("a 429 must carry Retry-After");
    let secs: u64 = retry.1.parse().expect("Retry-After is whole seconds");
    assert!(secs >= 1, "never tells a client to retry immediately");
}

/// A batch above the cap is refused as a whole, with the limit named so the
/// caller can adjust rather than guess.
#[tokio::test]
async fn an_oversized_batch_is_refused() {
    let up = FakeUpstream::new();
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let big: Vec<Value> = (0..101)
        .map(|i| call(i, "eth_blockNumber", json!([])))
        .collect();
    let (status, _, _) = post(&app, Value::Array(big)).await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(up.calls(), 0);
}

/// An unconfigured chain is a 404, not a 502: the caller has the wrong URL, and
/// telling them so is more useful than a generic upstream failure.
#[tokio::test]
async fn an_unknown_chain_is_a_404() {
    let up = FakeUpstream::new();
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let mut req = Request::builder()
        .method("POST")
        .uri("/v1/999999")
        .header("content-type", "application/json")
        .body(Body::from(
            call(1, "eth_blockNumber", json!([])).to_string(),
        ))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "203.0.113.7:5000".parse::<std::net::SocketAddr>().unwrap(),
    ));

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// A notification wants no answer, so there is nowhere to put the result the
/// upstream budget would have bought.
#[tokio::test]
async fn a_notification_is_refused() {
    let up = FakeUpstream::new();
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (status, _, _) = post(&app, json!({"jsonrpc":"2.0","method":"eth_blockNumber"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(up.calls(), 0);
}

/// The one method answered entirely from config. A chain id is immutable, so
/// an upstream call for it would be redundant.
#[tokio::test]
async fn the_chain_id_never_reaches_the_upstream() {
    let up = FakeUpstream::new();
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (status, body, _) = post(&app, call(1, "eth_chainId", json!([]))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"], format!("0x{CHAIN:x}"));
    assert_eq!(up.calls(), 0);
}

/// The cold-start path. `eth_getLogs` with an open `toBlock` cannot be
/// range-checked without a head, so the service resolves one first rather than
/// refusing a legitimate first request.
#[tokio::test]
async fn an_open_ended_get_logs_resolves_the_head_first() {
    let up = FakeUpstream::new();
    up.answer("eth_blockNumber", json!("0xf4240"));
    up.answer("eth_getLogs", json!([]));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (status, body, _) = post(
        &app,
        call(
            1,
            "eth_getLogs",
            json!([{
                "address": MASP.to_string(),
                // 3600 blocks below the head, as the SDK's own window is.
                "fromBlock": "0xf3230",
                "toBlock": "latest"
            }]),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], json!([]), "{body}");
}

/// The cost of the range cap being real: a scan the SDK would never issue is
/// refused rather than forwarded to a paid archive node.
#[tokio::test]
async fn a_chain_wide_log_scan_is_refused() {
    let up = FakeUpstream::new();
    up.answer("eth_blockNumber", json!("0xf4240"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (status, body, _) = post(
        &app,
        call(
            1,
            "eth_getLogs",
            json!([{"address": MASP.to_string(), "fromBlock": "0x1", "toBlock": "latest"}]),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32602, "{body}");
    assert_eq!(
        up.batches()
            .iter()
            .filter(|b| b.contains(&"eth_getLogs".to_string()))
            .count(),
        0,
        "the scan never reached the upstream"
    );
}

/// The free-work vector. Deserializing a 256 KB body is the most expensive
/// thing an unauthenticated caller can ask of this process, and it necessarily
/// happens before the method weights that fund everything else are knowable.
///
/// Charged at the door, a flood of oversized garbage runs out of budget. Left
/// uncharged, it is answered with a parse error at no cost to the sender, on a
/// connection it can reuse immediately.
#[tokio::test]
async fn a_flood_of_oversized_bodies_is_rate_limited() {
    let up = FakeUpstream::new();
    let tight = Quotas {
        client: ClientQuotas {
            units_per_second: 30,
            burst_units: 180,
            long_units: 3_000,
            long_window: std::time::Duration::from_secs(300),
        },
        global: GlobalQuotas {
            units_per_second: 1_000,
            burst_units: 1_000,
        },
    };
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, tight);

    // At the body cap, and not valid JSON — the point is that it costs the
    // sender something even though it can never be served.
    let junk = "x".repeat(256 * 1024);

    let mut accepted = 0;
    let mut limited = false;
    for _ in 0..60 {
        let (status, _, _) = post_raw(&app, junk.clone()).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
        assert_eq!(status, StatusCode::BAD_REQUEST, "junk is not a request");
        accepted += 1;
    }

    assert!(limited, "an oversized-body flood must run out of budget");
    assert!(
        accepted <= 20,
        "a 180-unit burst must not buy {accepted} oversized parses"
    );
    assert_eq!(up.calls(), 0, "none of this should reach the upstream");
}

/// The cold-start herd. While the tip is unknown every `eth_getLogs` needs a
/// head resolved before it can be range-checked, and resolving it per request
/// would be one extra paid call apiece.
///
/// Normally that window is a cold start. It is permanent if an upstream keeps
/// answering with a block number the tip tracker cannot parse — which is
/// exactly when an uncoalesced call would hurt most.
#[tokio::test]
async fn a_cold_start_get_logs_burst_resolves_the_head_once() {
    let up = FakeUpstream::with_delay(std::time::Duration::from_millis(50));
    up.answer("eth_blockNumber", json!("0xf4240"));
    up.answer("eth_getLogs", json!([]));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let open_ended = |id: u64| {
        call(
            id,
            "eth_getLogs",
            json!([{
                "address": MASP.to_string(),
                "fromBlock": "0xf3230",
                "toBlock": "latest"
            }]),
        )
    };

    // Every one of these arrives with the tip still unknown.
    let mut set = tokio::task::JoinSet::new();
    for id in 0..50u64 {
        let app = app.clone();
        set.spawn(async move { post(&app, open_ended(id)).await.0 });
    }
    while let Some(res) = set.join_next().await {
        assert_eq!(res.unwrap(), StatusCode::OK);
    }

    let head_calls = up
        .batches()
        .iter()
        .filter(|b| b.contains(&"eth_blockNumber".to_string()))
        .count();
    assert_eq!(
        head_calls, 1,
        "a cold-start burst must resolve the head once, not once per request"
    );
}

/// A block hash the upstream does not know yet — a load-balanced node one
/// block behind — answers `null`. Caching that would hide the block for the
/// whole TTL; a found block deep enough to be final is held.
#[tokio::test]
async fn a_block_by_hash_caches_a_found_block_but_never_null() {
    let hash = format!("0x{}", "ab".repeat(32));
    let up = FakeUpstream::new();
    up.answer("eth_blockNumber", json!("0xf4240"));
    up.answer("eth_getBlockByHash", json!(null));
    up.answer(
        "eth_getBlockByHash",
        json!({"number": "0x64", "hash": hash}),
    );
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    post(&app, call(0, "eth_blockNumber", json!([]))).await;
    let before = up.calls();

    let by_hash = |id| call(id, "eth_getBlockByHash", json!([hash, false]));
    let (_, body, _) = post(&app, by_hash(1)).await;
    assert_eq!(body["result"], Value::Null);

    let (_, body, _) = post(&app, by_hash(2)).await;
    assert_eq!(body["result"]["number"], "0x64", "null was not cached");

    post(&app, by_hash(3)).await;
    assert_eq!(
        up.calls() - before,
        2,
        "the found block is served from cache"
    );
}

/// A block body carries its height as `number`. Reading `latest` must teach
/// the service the head, so an open-ended `eth_getLogs` right after needs no
/// head fetch of its own.
#[tokio::test]
async fn a_latest_block_sets_the_head() {
    let up = FakeUpstream::new();
    up.answer("eth_getBlockByNumber", json!({"number": "0xf4240"}));
    up.answer("eth_blockNumber", json!("0xf4240"));
    up.answer("eth_getLogs", json!([]));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    post(
        &app,
        call(1, "eth_getBlockByNumber", json!(["latest", false])),
    )
    .await;
    let (status, body, _) = post(
        &app,
        call(
            2,
            "eth_getLogs",
            json!([{"address": MASP.to_string(), "fromBlock": "0xf3230", "toBlock": "latest"}]),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"], json!([]), "{body}");
    assert!(
        !up.batches()
            .iter()
            .any(|b| b.contains(&"eth_blockNumber".to_string())),
        "the head came from the block, not a warm-up fetch: {:?}",
        up.batches()
    );
}

/// A revert is the chain's answer. Asked again inside the TTL it is served from
/// cache, still as an error, with the chain's own code and message.
#[tokio::test]
async fn an_eth_call_revert_is_cached_like_a_result() {
    let up = FakeUpstream::new();
    up.fail(
        "eth_call",
        json!({"code": 3, "message": "execution reverted: paused"}),
    );
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    for id in 1..=2 {
        let (status, body, _) = post(&app, token_balance(id)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], 3, "{body}");
        assert_eq!(body["error"]["message"], "execution reverted: paused");
    }
    assert_eq!(up.calls(), 1, "the revert was served from cache");
}

/// An error about the node — here a load-balanced upstream that has not seen
/// the block yet — must be asked again rather than served for a whole TTL.
#[tokio::test]
async fn a_node_error_is_not_cached() {
    let up = FakeUpstream::new();
    up.fail(
        "eth_call",
        json!({"code": -32000, "message": "header not found"}),
    );
    up.answer("eth_call", json!("0x2a"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (_, body, _) = post(&app, token_balance(1)).await;
    assert_eq!(body["error"]["code"], -32000, "{body}");

    let (_, body, _) = post(&app, token_balance(2)).await;
    assert_eq!(body["result"], "0x2a", "the retry reached the upstream");
    assert_eq!(up.calls(), 2);
}

/// An allowlisted `eth_call` at the tip.
fn token_balance(id: u64) -> Value {
    call(
        id,
        "eth_call",
        json!([{"to": TOKEN.to_string(), "data": BALANCE_OF}, "latest"]),
    )
}

/// `balanceOf(owner)` on the allowlisted token, at the tip.
fn balance_of(id: u64, owner: u64) -> Value {
    call(
        id,
        "eth_call",
        json!([{"to": TOKEN.to_string(), "data": format!("{BALANCE_OF}{owner:064x}")}, "latest"]),
    )
}

/// An `aggregate3` answer as the node would return it: one `(success, data)`
/// per packed call. Encoded by the proxy's own encoder, which its unit tests
/// hold to Multicall3's ABI.
fn packed(results: &[(bool, &str)]) -> Value {
    let outcomes = results
        .iter()
        .map(|(success, data)| (*success, data.parse().unwrap()))
        .collect();
    serde_json::from_str(rpc_proxy::domain::multicall::respond(outcomes).get()).unwrap()
}

/// Code at the Multicall3 address, as `eth_getCode` answers it.
const MULTICALL3_CODE: &str = "0x6080604052";

/// The provider bill. Two misses at one block are one upstream `eth_call`, and
/// each result is still cached under its own key — so a later lone read of
/// either is free.
#[tokio::test]
async fn eth_calls_at_one_block_go_upstream_as_one_multicall() {
    let up = FakeUpstream::new();
    up.answer("eth_getCode", json!(MULTICALL3_CODE));
    up.answer("eth_call", packed(&[(true, "0x01"), (true, "0x02")]));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (status, body, _) = post(&app, json!([balance_of(1, 1), balance_of(2, 2)])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["result"], "0x01", "{body}");
    assert_eq!(body[1]["result"], "0x02", "{body}");
    assert_eq!(
        up.batches(),
        vec![
            vec!["eth_getCode".to_string()],
            vec!["eth_call".to_string()]
        ],
        "probed once, then both calls in one"
    );

    let (_, body, _) = post(&app, balance_of(3, 2)).await;
    assert_eq!(body["result"], "0x02");
    assert_eq!(up.calls(), 2, "each packed result is cached on its own key");
}

/// A bare anvil has no Multicall3. Calls go out one by one, and the chain is
/// asked only once.
#[tokio::test]
async fn a_chain_without_multicall_forwards_calls_one_by_one() {
    let up = FakeUpstream::new();
    up.answer("eth_getCode", json!("0x"));
    up.answer("eth_call", json!("0x2a"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    post(&app, json!([balance_of(1, 1), balance_of(2, 2)])).await;
    let (_, body, _) = post(&app, json!([balance_of(3, 3), balance_of(4, 4)])).await;
    assert_eq!(body[1]["result"], "0x2a", "{body}");

    let two = vec!["eth_call".to_string(), "eth_call".to_string()];
    assert_eq!(
        up.batches(),
        vec![vec!["eth_getCode".to_string()], two.clone(), two],
        "never packed, never re-probed"
    );
}

/// A failed pack member might be a revert or might be the pack's shared gas
/// running out. It is asked again on its own, so the caller sees — and the
/// cache keeps — the node's own verdict.
#[tokio::test]
async fn a_failed_pack_member_is_asked_again_unpacked() {
    let up = FakeUpstream::new();
    up.answer("eth_getCode", json!(MULTICALL3_CODE));
    up.answer("eth_call", packed(&[(true, "0x01"), (false, "0x")]));
    up.fail(
        "eth_call",
        json!({"code": 3, "message": "execution reverted: nope"}),
    );
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (_, body, _) = post(&app, json!([balance_of(1, 1), balance_of(2, 2)])).await;
    assert_eq!(body[0]["result"], "0x01", "{body}");
    assert_eq!(body[1]["error"]["code"], 3, "{body}");
    assert_eq!(body[1]["error"]["message"], "execution reverted: nope");
    assert_eq!(
        up.batches().last().unwrap(),
        &vec!["eth_call".to_string()],
        "only the failed member was asked again"
    );
    assert_eq!(up.calls(), 3);
}

/// A pack that does not unpack — `0x` because the code went away under a
/// running proxy, as on a reset dev chain — is not the answer to anything. Its
/// members are asked again unpacked.
#[tokio::test]
async fn a_pack_that_does_not_unpack_is_asked_again_unpacked() {
    let up = FakeUpstream::new();
    up.answer("eth_getCode", json!(MULTICALL3_CODE));
    up.answer("eth_call", json!("0x"));
    up.answer("eth_call", json!("0x2a"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (_, body, _) = post(&app, json!([balance_of(1, 1), balance_of(2, 2)])).await;
    assert_eq!(body[0]["result"], "0x2a", "{body}");
    assert_eq!(body[1]["result"], "0x2a", "{body}");
    assert_eq!(
        up.batches().last().unwrap(),
        &vec!["eth_call".to_string(), "eth_call".to_string()]
    );
}

/// Inside a pack `msg.sender` is Multicall3. A call that names its sender is
/// asking a different question, so it is never packed — and a batch of only
/// such calls never costs a probe.
#[tokio::test]
async fn a_call_naming_its_sender_is_not_packed() {
    let up = FakeUpstream::new();
    up.answer("eth_call", json!("0x2a"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let from = |id: u64, owner: u64| {
        call(
            id,
            "eth_call",
            json!([{
                "to": TOKEN.to_string(),
                "from": "0x5555555555555555555555555555555555555555",
                "data": format!("{BALANCE_OF}{owner:064x}"),
            }, "latest"]),
        )
    };
    post(&app, json!([from(1, 1), from(2, 2)])).await;
    assert_eq!(
        up.batches(),
        vec![vec!["eth_call".to_string(), "eth_call".to_string()]]
    );
}

/// A client's `aggregate3` over `(target, calldata, allowFailure)`, at the tip.
fn client_multicall(id: u64, calls: &[(Address, String, bool)]) -> Value {
    use alloy::sol_types::SolCall;
    use chain_types::abi::{IMulticall3, MULTICALL3};

    let calls = calls
        .iter()
        .map(|(target, data, allow)| IMulticall3::Call3 {
            target: *target,
            allowFailure: *allow,
            callData: data.parse().unwrap(),
        })
        .collect();
    let data = alloy::primitives::Bytes::from(IMulticall3::aggregate3Call { calls }.abi_encode());
    call(
        id,
        "eth_call",
        json!([{"to": MULTICALL3.to_string(), "data": data.to_string()}, "latest"]),
    )
}

/// `balanceOf(owner)` calldata.
fn balance_of_data(owner: u64) -> String {
    format!("{BALANCE_OF}{owner:064x}")
}

/// Decode an `aggregate3` result into `(success, returnData)` pairs.
fn outcomes(result: &Value) -> Vec<(bool, String)> {
    use alloy::sol_types::SolCall;
    use chain_types::abi::IMulticall3;

    let bytes: alloy::primitives::Bytes = result.as_str().expect("a hex result").parse().unwrap();
    IMulticall3::aggregate3Call::abi_decode_returns(&bytes, true)
        .unwrap()
        .returnData
        .into_iter()
        .map(|r| (r.success, r.returnData.to_string()))
        .collect()
}

/// A client multicall shares the cache with plain calls in both directions: a
/// call already cached is not fetched again, and what the multicall fetched
/// serves a later plain read.
#[tokio::test]
async fn a_client_multicall_is_served_from_its_calls_own_cache_entries() {
    let up = FakeUpstream::new();
    up.answer("eth_call", json!("0x01"));
    up.answer("eth_call", json!("0x02"));
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    // Owner 1 is read on its own first.
    let (_, body, _) = post(&app, balance_of(1, 1)).await;
    assert_eq!(body["result"], "0x01");

    let (status, body, _) = post(
        &app,
        client_multicall(
            2,
            &[
                (TOKEN, balance_of_data(1), false),
                (TOKEN, balance_of_data(2), false),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        outcomes(&body["result"]),
        vec![(true, "0x01".into()), (true, "0x02".into())],
        "{body}"
    );
    assert_eq!(up.calls(), 2, "only owner 2 was fetched for the multicall");

    let (_, body, _) = post(&app, balance_of(3, 2)).await;
    assert_eq!(body["result"], "0x02");
    assert_eq!(up.calls(), 2, "the multicall's result served a plain read");
}

/// Unbundling must not open a way around the allowlist: a multicall reaching
/// an unlisted contract is refused whole, names the offending call, and costs
/// nothing upstream.
#[tokio::test]
async fn a_client_multicall_reaching_an_unlisted_contract_is_refused() {
    let up = FakeUpstream::new();
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let stranger = address!("9999999999999999999999999999999999999999");
    let (status, body, _) = post(
        &app,
        client_multicall(
            1,
            &[
                (TOKEN, balance_of_data(1), true),
                (stranger, balance_of_data(1), true),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32602, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("aggregate3 call 1"),
        "{body}"
    );
    assert_eq!(up.calls(), 0);
}

/// Only `aggregate3` is unbundled. Anything else at Multicall3 is refused
/// legibly rather than forwarded whole.
#[tokio::test]
async fn another_multicall3_function_is_refused() {
    let up = FakeUpstream::new();
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let try_aggregate = call(
        1,
        "eth_call",
        json!([{"to": chain_types::abi::MULTICALL3.to_string(), "data": "0xbce38bd7"}, "latest"]),
    );
    let (_, body, _) = post(&app, try_aggregate).await;
    assert_eq!(body["error"]["code"], -32602, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("only aggregate3"),
        "{body}"
    );
    assert_eq!(up.calls(), 0);
}

/// A call the client allowed to fail is reported in place, with the node's
/// own revert data, as `aggregate3` would report it.
#[tokio::test]
async fn a_revert_the_client_allowed_is_reported_in_place() {
    let up = FakeUpstream::new();
    up.answer("eth_getCode", json!("0x"));
    up.answer("eth_call", json!("0x01"));
    up.fail(
        "eth_call",
        json!({"code": 3, "message": "execution reverted", "data": "0xdeadbeef"}),
    );
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (_, body, _) = post(
        &app,
        client_multicall(
            1,
            &[
                (TOKEN, balance_of_data(1), false),
                (TOKEN, balance_of_data(2), true),
            ],
        ),
    )
    .await;
    assert_eq!(
        outcomes(&body["result"]),
        vec![(true, "0x01".into()), (false, "0xdeadbeef".into())],
        "{body}"
    );
}

/// A call the client did not allow to fail reverts the whole multicall, with
/// the reason Multicall3 itself gives.
#[tokio::test]
async fn a_revert_the_client_did_not_allow_reverts_the_multicall() {
    let up = FakeUpstream::new();
    up.fail(
        "eth_call",
        json!({"code": 3, "message": "execution reverted", "data": "0xdeadbeef"}),
    );
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (_, body, _) = post(
        &app,
        client_multicall(1, &[(TOKEN, balance_of_data(1), false)]),
    )
    .await;
    assert_eq!(body["error"]["code"], 3, "{body}");
    assert_eq!(
        body["error"]["message"],
        "execution reverted: Multicall3: call failed"
    );
}

/// A plain revert passes its data through, which is what a client decodes a
/// custom error from.
#[tokio::test]
async fn a_revert_carries_its_data() {
    let up = FakeUpstream::new();
    up.fail(
        "eth_call",
        json!({"code": 3, "message": "execution reverted", "data": "0xDEADBEEF", "extra": 1}),
    );
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, generous());

    let (_, body, _) = post(&app, token_balance(1)).await;
    assert_eq!(
        body["error"],
        json!({"code": 3, "message": "execution reverted", "data": "0xdeadbeef"}),
        "data normalised, anything else dropped"
    );
}

/// A multicall is charged for every call it carries. Charged as one read, a
/// single request could spend a batch's worth of upstream work.
///
/// Here the charge exceeds the whole burst, so the request can never be
/// admitted: it is a 400 naming the cost and the limit, which viem does not
/// retry — not a 429 inviting a retry that would fail identically.
#[tokio::test]
async fn a_client_multicall_is_charged_per_call() {
    let up = FakeUpstream::new();
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, twenty_unit_burst());

    // Ten calls at three units apiece is over a twenty-unit burst; one call
    // would have been well under it.
    let calls: Vec<(Address, String, bool)> =
        (0..10).map(|i| (TOKEN, balance_of_data(i), true)).collect();
    assert_never_admitted(&app, client_multicall(1, &calls)).await;
    assert_eq!(up.calls(), 0);
}

/// The same for a plain batch: heavier than the burst is never admissible.
#[tokio::test]
async fn a_batch_heavier_than_the_whole_burst_is_refused_without_retry() {
    let up = FakeUpstream::new();
    let app = app(Arc::clone(&up) as Arc<dyn Upstream>, twenty_unit_burst());

    // Ten calls at three units apiece against a twenty-unit burst.
    let batch: Vec<Value> = (0..10).map(|i| balance_of(i, i)).collect();
    assert_never_admitted(&app, Value::Array(batch)).await;
    assert_eq!(up.calls(), 0);
}

/// Per-client quotas with a burst too small for ten `eth_call`s at once.
fn twenty_unit_burst() -> Quotas {
    Quotas {
        client: ClientQuotas {
            units_per_second: 10,
            burst_units: 20,
            long_units: 1_000,
            long_window: std::time::Duration::from_secs(300),
        },
        ..generous()
    }
}

/// A request no wait can admit is a 400 without `Retry-After`: a status viem
/// does not retry, and no header inviting it to.
async fn assert_never_admitted(app: &Router, body: Value) {
    let (status, _, headers) = post(app, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        !headers.iter().any(|(k, _)| k == "retry-after"),
        "no retry is invited"
    );
}
