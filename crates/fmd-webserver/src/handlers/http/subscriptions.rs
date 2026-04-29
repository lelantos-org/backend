use crate::app::AppState;
use crate::domain::dto::CreateSubscription;
use crate::domain::error::AppResult;
use crate::domain::responses::SubscriptionOut;
use crate::services;
use axum::Json;
use axum::extract::{Path, State};

#[utoipa::path(
    get,
    path = "/v1/subscriptions",
    tag = "subscriptions",
    responses((status = 200, body = [SubscriptionOut]))
)]
#[tracing::instrument(skip(st))]
pub async fn list_subscriptions(
    State(st): State<AppState>,
) -> AppResult<Json<Vec<SubscriptionOut>>> {
    Ok(Json(services::subscriptions::list(&st).await?))
}

#[utoipa::path(
    post,
    path = "/v1/subscriptions",
    tag = "subscriptions",
    request_body = CreateSubscription,
    responses((status = 200, body = SubscriptionOut))
)]
#[tracing::instrument(skip(st, body), fields(gamma = body.gamma))]
pub async fn create_subscription(
    State(st): State<AppState>,
    Json(body): Json<CreateSubscription>,
) -> AppResult<Json<SubscriptionOut>> {
    Ok(Json(
        services::subscriptions::create(&st, &body.detection_key_hex, body.gamma).await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/v1/subscriptions/{id}",
    tag = "subscriptions",
    params(("id" = i64, Path, description = "subscription id")),
    responses((status = 200, description = "ok"))
)]
#[tracing::instrument(skip(st))]
pub async fn delete_subscription(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<&'static str> {
    services::subscriptions::delete(&st, id).await?;
    Ok("ok")
}
