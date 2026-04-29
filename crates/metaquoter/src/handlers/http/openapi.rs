use crate::domain::models::{Quote, QuoteRequest, Venue};
use crate::handlers::http as handlers;
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "metaquoter",
        description = "Quote-aggregation backend for shielded swaps. Races venue-specific \
                       quoters and returns the best route + minOut for the SDK to bind into \
                       the SwapWrapper proof bundles."
    ),
    paths(handlers::quotes::post_quote),
    components(schemas(QuoteRequest, Quote, Venue)),
    tags((name = "quotes", description = "Best-route quotes across allowlisted venues")),
)]
pub struct ApiDoc;
