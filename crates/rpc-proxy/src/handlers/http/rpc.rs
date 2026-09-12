//! The JSON-RPC endpoint.

use crate::adapters::ratelimit::{ClientKey, client_key};
use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::jsonrpc::{Incoming, Request, Response, VERSION};
use crate::services::proxy::ChainService;
use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use serde_json::Value;
use std::net::SocketAddr;
use tracing::{Instrument, Span, debug, field, info_span};

/// `POST /v1/{chain_id}`.
///
/// Takes the body as bytes rather than `Json<Incoming>`. The extractor would
/// deserialize before the rate limiter has charged for it, and the charge has
/// to come first: the parse is the most expensive thing an unauthenticated
/// caller can ask of this process. It also lets a body that is not a request
/// be refused with this service's own message rather than axum's.
///
/// A body that does not parse is an [`AppError::BadRequest`] — an HTTP status,
/// not a `-32700` object. Nothing has been read at that point, so there is no
/// `id` to answer under. JSON-RPC error objects with HTTP 200 are reserved for
/// a request that parsed and was then refused; see
/// [`crate::domain::jsonrpc`].
pub async fn post_rpc(
    State(state): State<AppState>,
    Path(chain_id): Path<u64>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> AppResult<axum::response::Response> {
    let chain = state
        .chains
        .get(&chain_id)
        .ok_or(AppError::UnsupportedChain(chain_id))?;

    let client = client_key(&headers, peer, &state.trusted_header);

    // Every line logged while serving this request carries the client's digest
    // — never its address — and how many calls it made, so one caller's
    // refusals and failures can be told apart from everyone's.
    let span = info_span!("rpc", client = %client.digest(), calls = field::Empty);
    handle(&state, chain, client, &body).instrument(span).await
}

async fn handle(
    state: &AppState,
    chain: &ChainService,
    client: ClientKey,
    body: &[u8],
) -> AppResult<axum::response::Response> {
    // Before the parse, not after: deserializing up to 256 KB is the most
    // expensive thing an unauthenticated caller can ask of this process, and
    // what a request costs is not knowable until it has happened.
    state
        .client_limiter
        .admit(client, body.len())
        .inspect_err(|_| debug!(bytes = body.len(), "refused at admission"))?;

    match Incoming::parse(body, state.max_batch)? {
        Incoming::Single(req) => {
            Span::current().record("calls", 1);
            check(std::slice::from_ref(&req))?;
            let out = chain.serve(std::slice::from_ref(&req), client).await?;
            let one = out
                .into_iter()
                .next()
                .ok_or_else(|| AppError::Internal("no response for a single request".into()))?;
            Ok(Json(one).into_response())
        }
        Incoming::Batch(reqs) => {
            Span::current().record("calls", reqs.len());
            check(&reqs)?;
            let out: Vec<Response> = chain.serve(&reqs, client).await?;
            Ok(Json(out).into_response())
        }
    }
}

/// Protocol-level checks that apply before any method is looked at.
fn check(reqs: &[Request]) -> AppResult<()> {
    for r in reqs {
        // A notification wants no answer, so there is nowhere to put a result.
        // Accepting one would mean spending upstream budget with no way to
        // return what it bought.
        if r.id.is_none() {
            return Err(AppError::BadRequest(
                "notifications are not supported; every call must carry an id".into(),
            ));
        }
        if r.jsonrpc != VERSION {
            return Err(AppError::BadRequest(format!(
                "unsupported jsonrpc version: {:?}",
                r.jsonrpc
            )));
        }
        // An id must be a scalar. An object or array id is legal-ish but is
        // echoed back verbatim, and there is no reason to reflect arbitrary
        // caller-controlled structure.
        if matches!(r.id, Some(Value::Object(_)) | Some(Value::Array(_))) {
            return Err(AppError::BadRequest("id must be a string or number".into()));
        }
    }
    Ok(())
}
