//! JSON-RPC 2.0 wire types, limited to the subset this proxy speaks.
//!
//! Results are carried as [`RawValue`]. An `eth_getLogs` answer can be hundreds
//! of kilobytes and is only ever inspected for `null` and `blockNumber`, so
//! holding the upstream's own bytes makes a cache hit an `Arc` clone instead of
//! a parse and a re-serialize.
//!
//! A request refused by the allowlist returns HTTP 200 carrying a JSON-RPC
//! error object. viem's `shouldRetry` does not retry `-32601`/`-32602`, so the
//! caller fails fast with a legible message; an HTTP 4xx would instead surface
//! as an opaque `HttpRequestError` and may be retried. HTTP statuses are
//! reserved for [`crate::domain::error`].

use crate::domain::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use std::sync::Arc;

/// The only protocol version accepted.
pub const VERSION: &str = "2.0";

// The standard JSON-RPC 2.0 error codes this endpoint returns. A body that is
// not a request at all is an `AppError` with an HTTP status rather than a
// `-32700`; see [`crate::domain::error`].
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

/// A pre-serialized `result`, shared between the cache and every response built
/// from it.
///
/// `Arc<RawValue>` rather than `Arc<str>`: the upstream envelope is parsed once
/// on the miss path, and this keeps that parse's output instead of re-parsing a
/// string on every hit.
pub type CachedResult = Arc<RawValue>;

/// What one call came back with: what the cache holds, and what concurrent
/// callers of one key share.
///
/// An error is a variant here rather than a failure because the chain said it —
/// `execution reverted` is an answer. Whether one may be *cached* is a separate
/// question; see [`crate::domain::policy::caches_error`].
#[derive(Debug, Clone)]
pub enum Reply {
    Result(CachedResult),
    Error(RpcError),
}

/// One request. `params` is left as an unparsed [`Value`] because each method
/// validates its own shape; see [`crate::domain::allowlist`].
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
    /// Absent for a notification, which this proxy rejects: a caching proxy has
    /// nothing to say to a request that wants no answer, and accepting one would
    /// mean spending upstream budget with no way to return the result.
    #[serde(default)]
    pub id: Option<Value>,
}

impl Request {
    /// `params` as a slice, treating both absent and `null` as empty.
    ///
    /// viem omits `params` for `eth_chainId` and sends `[]` for others; both
    /// have to validate identically or the same logical call would be accepted
    /// from one client and refused from another.
    pub fn params(&self) -> &[Value] {
        positional(self.params.as_ref())
    }

    /// Whether `params` is a well-formed array (or absent), rather than the
    /// object form the spec also permits. No method here takes named params, and
    /// silently reading an object as empty would let `eth_getLogs` past its
    /// range check.
    pub fn params_are_positional(&self) -> bool {
        matches!(
            &self.params,
            None | Some(Value::Null) | Some(Value::Array(_))
        )
    }
}

/// Raw `params` as a slice; see [`Request::params`]. Separate so a call held
/// apart from its request reads its params identically.
pub fn positional(params: Option<&Value>) -> &[Value] {
    match params {
        Some(Value::Array(a)) => a,
        _ => &[],
    }
}

/// A single request or a batch, distinguished by the JSON shape.
#[derive(Debug, Clone)]
pub enum Incoming {
    Single(Request),
    Batch(Vec<Request>),
}

impl Incoming {
    /// Parse a request body, refusing a batch larger than `max_batch`.
    ///
    /// Hand-rolled rather than `#[serde(untagged)]`, which buffers the whole
    /// document into an owned tree and then deserializes it a second time —
    /// twice over, on a 256 KB body from an unauthenticated caller. One byte
    /// decides the same question.
    ///
    /// The batch arm scans structurally before it parses, so an oversized batch
    /// is refused without building a `params` tree for any of its entries. The
    /// `&RawValue` slices borrow from `body`, so that pass copies nothing. The
    /// size checks live here for the same reason: they decide how much of this
    /// body is worth looking at.
    pub fn parse(body: &[u8], max_batch: usize) -> AppResult<Self> {
        let malformed =
            |e: serde_json::Error| AppError::BadRequest(format!("not a JSON-RPC request: {e}"));

        match body.iter().find(|b| !b.is_ascii_whitespace()) {
            Some(b'[') => {
                let raw: Vec<&RawValue> = serde_json::from_slice(body).map_err(malformed)?;
                // An empty batch is explicitly invalid per the spec, and
                // answering `[]` would look like every call succeeded.
                if raw.is_empty() {
                    return Err(AppError::BadRequest("empty batch".into()));
                }
                if raw.len() > max_batch {
                    return Err(AppError::BatchTooLarge {
                        got: raw.len(),
                        limit: max_batch,
                    });
                }
                raw.into_iter()
                    .map(|r| serde_json::from_str(r.get()).map_err(malformed))
                    .collect::<AppResult<Vec<Request>>>()
                    .map(Incoming::Batch)
            }
            // Not an array: let serde say why it is not a request either.
            _ => serde_json::from_slice(body)
                .map_err(malformed)
                .map(Incoming::Single),
        }
    }
}

/// An error object.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    /// Revert data, as `0x` hex. What viem decodes a custom error from, and
    /// what a multicall reports for a call that failed in place.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        // Names the method so an SDK change that adds a read shows up as a
        // legible console error rather than an unexplained failure.
        Self::new(
            METHOD_NOT_FOUND,
            format!("method not supported by this endpoint: {method}"),
        )
    }

    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, detail)
    }
}

/// One response.
///
/// `result` and `error` are mutually exclusive; the constructors are the only
/// way to build one, so a response carrying both cannot be expressed.
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    /// Echoed from the request. A response to a request whose `id` could not be
    /// read carries `null`, per the spec.
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<CachedResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn result(id: Value, result: CachedResult) -> Self {
        Self {
            jsonrpc: VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: VERSION,
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// A response as it arrives from upstream: `result` kept raw, `error` passed
/// through untouched.
///
/// An upstream `error` is an **answer**, not a failure — `execution reverted` is
/// a verdict about the call, and returning it verbatim is what lets the SDK
/// distinguish a reverting read from an unreachable node. Nothing here retries
/// on it or rewrites it.
#[derive(Debug, Clone, Deserialize)]
pub struct UpstreamResponse {
    #[serde(default)]
    pub id: Value,
    /// `None` only when the field is **absent**. A present `"result": null` is
    /// `Some`, holding the literal `null`.
    ///
    /// This distinction is load-bearing: `eth_getTransactionReceipt` answers
    /// `null` for a transaction that has not been mined, which is the normal
    /// response on every tick of a deposit's receipt poll. A plain
    /// `Option<Box<RawValue>>` collapses the two — serde's `Option` intercepts
    /// `null` before `RawValue` sees it — so a pending receipt would decode as
    /// a malformed response and fail the whole confirmation.
    #[serde(default, deserialize_with = "present_or_absent")]
    pub result: Option<Box<RawValue>>,
    #[serde(default)]
    pub error: Option<Value>,
}

/// Deserialize a field as `Some`, including when its value is `null`, leaving
/// `None` to mean the field was not there at all. See [`UpstreamResponse::result`].
fn present_or_absent<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(d).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: serde_json::Value) -> Incoming {
        Incoming::parse(v.to_string().as_bytes(), 100).expect("parses")
    }

    #[test]
    fn a_single_request_and_a_batch_are_told_apart_by_shape() {
        assert!(matches!(
            parse(json!({"jsonrpc":"2.0","method":"eth_chainId","id":1})),
            Incoming::Single(_)
        ));
        assert!(matches!(
            parse(json!([{"jsonrpc":"2.0","method":"eth_chainId","id":1}])),
            Incoming::Batch(_)
        ));
    }

    /// The shape is decided by the first non-whitespace byte, so a body a
    /// client pretty-printed must still be read as a batch.
    #[test]
    fn leading_whitespace_does_not_hide_the_shape() {
        let body = b"  \r\n\t [{\"jsonrpc\":\"2.0\",\"method\":\"eth_chainId\",\"id\":1}]";
        assert!(matches!(
            Incoming::parse(body, 100).unwrap(),
            Incoming::Batch(_)
        ));
    }

    /// An empty body is a parse error, not an empty batch.
    #[test]
    fn an_empty_body_is_malformed() {
        for body in [b"".as_slice(), b"   ".as_slice()] {
            assert!(matches!(
                Incoming::parse(body, 100),
                Err(AppError::BadRequest(_))
            ));
        }
    }

    /// Answering `[]` would look like every call in the batch succeeded.
    #[test]
    fn an_empty_batch_is_refused() {
        assert!(matches!(
            Incoming::parse(b"[]", 100),
            Err(AppError::BadRequest(_))
        ));
    }

    /// The size check must happen on the structural scan, before any entry's
    /// `params` tree is built — that parse is the cost the check exists to
    /// avoid, so doing it first would defeat the point.
    ///
    /// Pinned by making every entry unparseable as a `Request`: reaching the
    /// `BatchTooLarge` verdict proves none of them was parsed.
    #[test]
    fn an_oversized_batch_is_refused_before_its_entries_are_parsed() {
        let body = format!("[{}]", vec!["12345"; 101].join(","));
        assert!(matches!(
            Incoming::parse(body.as_bytes(), 100),
            Err(AppError::BatchTooLarge {
                got: 101,
                limit: 100
            })
        ));
    }

    /// A batch at the limit is still served, and its entries do get parsed.
    #[test]
    fn a_batch_at_the_limit_is_accepted() {
        let one = r#"{"jsonrpc":"2.0","method":"eth_chainId","id":1}"#;
        let body = format!("[{}]", vec![one; 100].join(","));
        match Incoming::parse(body.as_bytes(), 100).unwrap() {
            Incoming::Batch(v) => assert_eq!(v.len(), 100),
            Incoming::Single(_) => panic!("should be a batch"),
        }
    }

    /// viem omits `params` entirely for no-argument methods and sends `[]`
    /// elsewhere. Both must read as empty, or the same call would be accepted
    /// from one client and refused from another.
    #[test]
    fn absent_and_empty_params_both_read_as_empty() {
        let omitted: Request =
            serde_json::from_value(json!({"jsonrpc":"2.0","method":"eth_chainId","id":1})).unwrap();
        let empty: Request = serde_json::from_value(
            json!({"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}),
        )
        .unwrap();

        assert!(omitted.params().is_empty());
        assert!(empty.params().is_empty());
        assert!(omitted.params_are_positional());
        assert!(empty.params_are_positional());
    }

    /// The spec also allows named params. No method here takes them, and
    /// reading an object as an empty array would slip an `eth_getLogs` past its
    /// range check with no `fromBlock` to test.
    #[test]
    fn object_params_are_not_positional() {
        let r: Request = serde_json::from_value(
            json!({"jsonrpc":"2.0","method":"eth_getLogs","params":{"fromBlock":"0x1"},"id":1}),
        )
        .unwrap();

        assert!(!r.params_are_positional());
        assert!(
            r.params().is_empty(),
            "must not read an object as arguments"
        );
    }

    /// A notification carries no `id`, so there is nowhere to put an answer.
    #[test]
    fn a_notification_has_no_id() {
        let r: Request =
            serde_json::from_value(json!({"jsonrpc":"2.0","method":"eth_chainId"})).unwrap();
        assert!(r.id.is_none());
    }

    /// A response carries `result` or `error`, never both and never neither —
    /// a client reading `result` first would treat `{"result":null,"error":{…}}`
    /// as a successful null.
    #[test]
    fn a_response_serializes_exactly_one_of_result_and_error() {
        let ok = Response::result(
            json!(1),
            RawValue::from_string("\"0x1\"".into()).unwrap().into(),
        );
        let text = serde_json::to_string(&ok).unwrap();
        assert!(text.contains(r#""result":"0x1""#), "{text}");
        assert!(!text.contains("error"), "{text}");

        let err = Response::error(json!(1), RpcError::method_not_found("eth_getProof"));
        let text = serde_json::to_string(&err).unwrap();
        assert!(text.contains(r#""code":-32601"#), "{text}");
        assert!(!text.contains("result"), "{text}");
        // The refused method is named, so the failure is diagnosable from a
        // browser console without server access.
        assert!(text.contains("eth_getProof"), "{text}");
    }

    /// A cached result is spliced back verbatim, so a large `eth_getLogs` body
    /// survives a round trip through the cache byte for byte.
    #[test]
    fn a_raw_result_round_trips_unchanged() {
        let body = r#"[{"address":"0xabc","topics":["0x01"]}]"#;
        let raw: CachedResult = RawValue::from_string(body.into()).unwrap().into();
        let text = serde_json::to_string(&Response::result(json!(7), raw)).unwrap();
        assert!(text.contains(body), "{text}");
    }

    /// A pending receipt is `"result": null` on every poll tick of every
    /// deposit. Reading it as an absent field makes it a decode failure and
    /// fails the receipt wait.
    #[test]
    fn a_present_null_result_is_not_an_absent_one() {
        let pending: UpstreamResponse =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":0,"result":null})).unwrap();
        assert!(pending.result.is_some(), "a null result is still a result");
        assert_eq!(pending.result.as_deref().unwrap().get(), "null");

        let malformed: UpstreamResponse =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":0})).unwrap();
        assert!(malformed.result.is_none(), "an absent result is absent");
    }
}
