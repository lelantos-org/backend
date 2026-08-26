use crate::app::AppState;
use crate::domain::dto::CreateSubscription;
use crate::domain::error::AppResult;
use crate::domain::responses::SubscriptionOut;
use crate::handlers::http::auth::CapabilityToken;
use crate::services;
use axum::Json;
use axum::extract::State;

#[utoipa::path(
    post,
    path = "/v1/subscriptions",
    tag = "subscriptions",
    request_body = CreateSubscription,
    responses((status = 200, body = SubscriptionOut))
)]
// `body` is skipped entirely: it carries both the detection key and the
// capability token.
#[tracing::instrument(skip(st, body), fields(gamma = body.gamma))]
pub async fn create_subscription(
    State(st): State<AppState>,
    Json(body): Json<CreateSubscription>,
) -> AppResult<Json<SubscriptionOut>> {
    Ok(Json(
        services::subscriptions::create(&st, &body.detection_key_hex, body.gamma, &body.token_hex)
            .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/v1/subscriptions",
    tag = "subscriptions",
    params(("Authorization" = String, Header, description = "`Bearer <subscription capability token, 32-byte hex>`")),
    responses((status = 200, description = "ok"), (status = 401, description = "missing or malformed Authorization header"))
)]
#[tracing::instrument(skip(st, token))]
pub async fn delete_subscription(
    State(st): State<AppState>,
    token: CapabilityToken,
) -> AppResult<&'static str> {
    services::subscriptions::delete(&st, token.hash()).await?;
    Ok("ok")
}
