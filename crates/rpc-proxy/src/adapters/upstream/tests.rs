use super::body::Body;
use super::http::{
    MAX_RETRY_AFTER_SECS, MIN_ATTEMPT, MIN_RETRY_AFTER_SECS, REQUEST_TIMEOUT, TOTAL_BUDGET,
    describe, retry_after,
};
use super::*;
use crate::domain::error::AppError;
use serde_json::json;
use std::time::Duration;

/// The API key lives in the endpoint URL, and reqwest prints the URL in
/// its errors. What is logged must keep the cause and lose the key.
#[tokio::test]
async fn a_transport_error_is_described_without_its_url() {
    // Port 1 on loopback: refused at once, with no network involved.
    let e = reqwest::Client::new()
        .post("http://127.0.0.1:1/v2/SECRETAPIKEY")
        .send()
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("SECRETAPIKEY"),
        "precondition: reqwest does print the URL"
    );

    let d = describe(e);
    assert!(!d.contains("SECRETAPIKEY"), "{d}");
    assert!(!d.contains("127.0.0.1:1/v2"), "{d}");
    assert!(d.contains(": "), "the underlying cause is kept: {d}");
}

fn calls(n: usize) -> Vec<OutboundCall<'static>> {
    (0..n)
        .map(|_| OutboundCall {
            method: "eth_blockNumber",
            params: None,
        })
        .collect()
}

/// A lone call goes out as an object, not a one-element array.
#[test]
fn a_single_call_is_not_sent_as_a_batch() {
    let c = calls(1);
    let text = serde_json::to_string(&match Body::new(&c) {
        Body::Single(e) => e,
        Body::Batch(_) => panic!("should be single"),
    })
    .unwrap();
    assert!(text.starts_with('{'), "{text}");
    assert!(text.contains(r#""jsonrpc":"2.0""#), "{text}");
}

/// The correctness case that matters most here. JSON-RPC lets a server
/// answer a batch in any order; zipping by position would hand one caller
/// another's answer, and both would be valid JSON.
#[test]
fn a_batch_response_is_reordered_by_id() {
    let c = calls(3);
    let body = Body::new(&c);
    let answers = body
        .decode(
            &json!([
                {"jsonrpc":"2.0","id":2,"result":"0xcc"},
                {"jsonrpc":"2.0","id":0,"result":"0xaa"},
                {"jsonrpc":"2.0","id":1,"result":"0xbb"}
            ])
            .to_string(),
        )
        .unwrap();

    let got: Vec<String> = answers
        .iter()
        .map(|a| match a {
            Answer::Result(r) => r.get().to_string(),
            Answer::Error(_) => panic!("unexpected error"),
        })
        .collect();
    assert_eq!(got, vec![r#""0xaa""#, r#""0xbb""#, r#""0xcc""#]);
}

/// An upstream error is an answer, carried through in its own slot rather
/// than failing the whole batch: one reverting `eth_call` must not take the
/// other nineteen reads down with it.
#[test]
fn one_error_in_a_batch_does_not_fail_the_others() {
    let c = calls(2);
    let answers = Body::new(&c)
        .decode(
            &json!([
                {"jsonrpc":"2.0","id":0,"error":{"code":3,"message":"execution reverted"}},
                {"jsonrpc":"2.0","id":1,"result":"0xbb"}
            ])
            .to_string(),
        )
        .unwrap();

    assert!(matches!(answers[0], Answer::Error(_)));
    assert!(matches!(answers[1], Answer::Result(_)));
}

/// A response set that cannot be matched to the requests is rejected rather
/// than guessed at.
#[test]
fn a_mismatched_batch_response_is_an_error() {
    let c = calls(2);
    let body = Body::new(&c);

    for bad in [
        // Too few.
        json!([{"jsonrpc":"2.0","id":0,"result":"0xaa"}]),
        // An id outside the batch.
        json!([
            {"jsonrpc":"2.0","id":0,"result":"0xaa"},
            {"jsonrpc":"2.0","id":9,"result":"0xbb"}
        ]),
        // The same id twice, leaving a slot unfilled.
        json!([
            {"jsonrpc":"2.0","id":0,"result":"0xaa"},
            {"jsonrpc":"2.0","id":0,"result":"0xbb"}
        ]),
        // Neither result nor error.
        json!([
            {"jsonrpc":"2.0","id":0},
            {"jsonrpc":"2.0","id":1,"result":"0xbb"}
        ]),
    ] {
        assert!(body.decode(&bad.to_string()).is_err(), "must reject {bad}");
    }
}

/// A `null` result is a real answer — an unmined receipt — and must survive
/// decoding rather than being read as a missing field.
#[test]
fn a_null_result_decodes_as_an_answer() {
    let c = calls(1);
    let answers = Body::new(&c)
        .decode(&json!({"jsonrpc":"2.0","id":0,"result":null}).to_string())
        .unwrap();
    match &answers[0] {
        Answer::Result(r) => assert_eq!(r.get(), "null"),
        Answer::Error(_) => panic!("null result is not an error"),
    }
}

/// With no archive endpoint configured, a historical read must report that
/// specifically rather than as a generic upstream failure.
#[tokio::test]
async fn an_archive_read_with_no_archive_endpoint_is_reported_as_such() {
    let up = HttpUpstream::new(
        1,
        vec![Endpoint {
            url: "http://127.0.0.1:1/".into(),
            archive: false,
            label: PRIMARY,
        }],
        64,
    )
    .unwrap();

    let err = up.call(&calls(1), true).await.unwrap_err();
    assert!(matches!(err, AppError::NoArchiveUpstream), "{err}");
}

#[test]
fn an_upstream_with_no_endpoints_is_a_config_error() {
    assert!(HttpUpstream::new(1, vec![], 64).is_err());
}

/// A cap of zero would make every call block forever, which looks like a
/// hung upstream rather than a config mistake.
#[test]
fn a_zero_concurrency_cap_is_a_config_error() {
    let ep = vec![Endpoint {
        url: "http://127.0.0.1:1/".into(),
        archive: true,
        label: PRIMARY,
    }];
    assert!(HttpUpstream::new(1, ep, 0).is_err());
}

fn endpoint(url: String) -> Vec<Endpoint> {
    vec![Endpoint {
        url,
        archive: true,
        label: PRIMARY,
    }]
}

/// An endpoint that refuses every request with HTTP 429 and this
/// `Retry-After`, as a provider over our quota does. Returns its URL.
async fn throttling(retry_after: &'static str) -> String {
    let app = axum::Router::new().fallback(move || async move {
        (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", retry_after)],
        )
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}/")
}

/// The provider said how long; the client is told the same.
#[tokio::test]
async fn an_upstream_429_is_throttled_with_its_retry_after() {
    let url = throttling("7").await;
    let up = HttpUpstream::new(1, endpoint(url), 1).unwrap();
    let err = up.call(&calls(1), false).await.unwrap_err();
    assert!(
        matches!(
            err,
            AppError::UpstreamThrottled {
                retry_after_secs: 7
            }
        ),
        "{err:?}"
    );
}

/// The quota is why the call failed even when the fallback then failed
/// some other way, and it is the failure a client can wait out.
#[tokio::test]
async fn a_throttled_primary_is_reported_over_a_failed_fallback() {
    let url = throttling("5").await;
    let endpoints = vec![
        Endpoint {
            url,
            archive: true,
            label: PRIMARY,
        },
        Endpoint {
            // Refused at once.
            url: "http://127.0.0.1:1/".into(),
            archive: true,
            label: FALLBACK,
        },
    ];
    let up = HttpUpstream::new(1, endpoints, 1).unwrap();
    let err = up.call(&calls(1), false).await.unwrap_err();
    assert!(
        matches!(
            err,
            AppError::UpstreamThrottled {
                retry_after_secs: 5
            }
        ),
        "{err:?}"
    );
}

/// Read in whole seconds, and clamped so neither "now" nor "an hour" is
/// passed on.
#[test]
fn retry_after_is_read_and_clamped() {
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

    let with = |v: &'static str| {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static(v));
        retry_after(&h)
    };
    assert_eq!(with("7"), 7);
    assert_eq!(with("0"), MIN_RETRY_AFTER_SECS);
    assert_eq!(with("3600"), MAX_RETRY_AFTER_SECS);
    assert_eq!(
        with("Wed, 21 Oct 2015 07:28:00 GMT"),
        MIN_RETRY_AFTER_SECS,
        "an HTTP-date is not guessed at"
    );
    assert_eq!(retry_after(&HeaderMap::new()), MIN_RETRY_AFTER_SECS);
}

/// The whole point of the cap: a call in flight holds a permit, so the ones
/// past the cap wait rather than opening another socket at a provider that
/// meters us.
///
/// Driven by a listener that accepts and never answers, rather than by a
/// closed port — a refused connection fails immediately and the call would
/// be over before it could be observed.
#[tokio::test]
async fn an_in_flight_call_holds_a_permit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // Held, not dropped: closing the socket would answer the request
        // with a transport error and end the call.
        let mut open = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            open.push(s);
        }
    });

    let up =
        std::sync::Arc::new(HttpUpstream::new(1, endpoint(format!("http://{addr}/")), 1).unwrap());
    assert_eq!(up.permits_available(), 1);

    let bg = std::sync::Arc::clone(&up);
    tokio::spawn(async move {
        let c = calls(1);
        let _ = bg.call(&c, false).await;
    });

    for _ in 0..200 {
        if up.permits_available() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("an in-flight call must hold the only permit");
}

/// A permit that is not returned wedges the service one slot at a time, and
/// the failure path is where that would happen — it is the one that does
/// not run to the end of the function.
#[tokio::test]
async fn a_failed_call_returns_its_permit() {
    // Port 1 refuses, so each call fails fast.
    let up = HttpUpstream::new(1, endpoint("http://127.0.0.1:1/".into()), 4).unwrap();

    for _ in 0..8 {
        let c = calls(1);
        assert!(up.call(&c, false).await.is_err());
    }

    assert_eq!(
        up.permits_available(),
        4,
        "a permit must not leak on the failure path"
    );
}

/// The two timeouts have to compose to less than the router's deadline, or
/// the fallback attempt is cancelled mid-flight — after it has already
/// opened a socket and spent a paid credit on an answer nobody receives.
#[test]
fn the_call_budget_fits_inside_the_request_deadline() {
    // `handlers::http::router::REQUEST_TIMEOUT`.
    assert!(
        TOTAL_BUDGET < Duration::from_secs(12),
        "the call budget must leave the router room to answer"
    );
    assert!(
        MIN_ATTEMPT < REQUEST_TIMEOUT && REQUEST_TIMEOUT <= TOTAL_BUDGET,
        "one full attempt must fit in the budget"
    );
}
