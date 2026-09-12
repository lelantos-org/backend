use crate::app::AppState;
use crate::domain::responses::ChainsResponse;
use axum::Json;
use axum::extract::State;
use std::sync::Arc;

/// Every chain this deployment serves, as the deployment describes it.
///
/// The registry a client boots from, paired with a relayer's own
/// `GET /chains`: this says what the chain is, that says what one relayer
/// will do on it. A wallet cross-checks `maspAddress` and `treeDepth` across the
/// two before trusting the relayer, which is why both sides publish addresses in
/// the same canonical EIP-55 form.
///
/// Served from the body resolved at boot; nothing here reads config or the
/// database per request.
#[utoipa::path(
    get,
    path = "/v1/chains",
    tag = "registry",
    responses((status = 200, body = ChainsResponse))
)]
pub async fn chains(State(st): State<AppState>) -> Json<Arc<ChainsResponse>> {
    Json(st.chains.clone())
}
