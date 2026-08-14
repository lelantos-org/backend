use crate::domain::dto::screen::{ScreenBatchRequest, ScreenRequest};
use crate::domain::responses::{EntryOut, MatchOut, ScreenOut};
use crate::domain::risk::RiskLevel;
use crate::handlers::http as handlers;
use crate::handlers::http::health::HealthOut;
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "risk-webserver",
        description = "Read-only address screening API"
    ),
    paths(
        handlers::health::health,
        handlers::screen::screen,
        handlers::screen::screen_batch,
        handlers::entries::list_entries,
    ),
    components(schemas(
        ScreenRequest,
        ScreenBatchRequest,
        ScreenOut,
        MatchOut,
        EntryOut,
        RiskLevel,
        HealthOut
    )),
    tags(
        (name = "health", description = "Health and build info"),
        (name = "screen", description = "Address risk screening"),
        (name = "entries", description = "Screening list contents"),
    )
)]
pub struct ApiDoc;
