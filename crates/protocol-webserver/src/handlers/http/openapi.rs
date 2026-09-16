use crate::domain::responses::{
    AssetOut, ChainOut, ChainsResponse, PriceOut, PricesResponse, YieldIndexAssetOut,
    YieldIndexResponse, YieldOut, YieldSampleOut,
};
use crate::handlers::http as handlers;
use crate::handlers::http::health::HealthOut;
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "protocol-webserver",
        description = "Deployment registry, asset catalog and spot prices"
    ),
    paths(
        handlers::health::health,
        handlers::chains::chains,
        handlers::assets::list_assets,
        handlers::prices::prices,
        handlers::yield_index::yield_index,
    ),
    components(schemas(
        HealthOut,
        ChainsResponse,
        ChainOut,
        AssetOut,
        YieldOut,
        PricesResponse,
        PriceOut,
        YieldIndexResponse,
        YieldIndexAssetOut,
        YieldSampleOut
    ))
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    /// The relayer shipped without a spec and its contract lived in the SDK's
    /// expectations instead. This service documents itself from day one, so the
    /// check is that every route it serves is actually in the document.
    #[test]
    fn the_spec_documents_every_endpoint() {
        let spec = ApiDoc::openapi();
        for path in [
            "/health",
            "/v1/chains",
            "/v1/assets",
            "/v1/prices",
            "/v1/yield-index",
        ] {
            assert!(
                spec.paths.paths.contains_key(path),
                "{path} is served but undocumented"
            );
        }
    }
}
