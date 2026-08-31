use crate::domain::responses::{
    AnonymitySetOut, AssetOut, ChainFlowOut, ChainLockedOut, CountPoint, FlowPoint, KindCounts,
    LockedAssetOut, PoolNotesOut, TreeAdvanceOut, TxKind, TxOut,
};
use crate::handlers::http as handlers;
use crate::handlers::http::health::HealthOut;
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(title = "explorer-webserver", description = "Explorer webserver API"),
    paths(
        handlers::health::health,
        handlers::assets::list_assets,
        handlers::tree_advances::list_tree_advances,
        handlers::tree_advances::tx_counts,
        handlers::tree_advances::chain_flows_24h,
        handlers::asset_flows::asset_flows,
        handlers::locked::locked_by_chain,
        handlers::transactions::recent_transactions,
        handlers::transactions::tx_kinds,
        handlers::anonymity_set::anonymity_set,
        handlers::pool_notes::pool_notes,
    ),
    components(schemas(
        AssetOut,
        TreeAdvanceOut,
        CountPoint,
        ChainFlowOut,
        ChainLockedOut,
        LockedAssetOut,
        FlowPoint,
        HealthOut,
        TxOut,
        TxKind,
        KindCounts,
        AnonymitySetOut,
        PoolNotesOut
    )),
    tags(
        (name = "health", description = "Health and build info"),
        (name = "assets", description = "Assets"),
        (name = "tree-advances", description = "Tree advances"),
        (name = "asset-flows", description = "Per-token deposit/withdraw flows"),
        (name = "locked", description = "Escrowed balances per chain: all-time deposits minus withdrawals"),
        (name = "transactions", description = "Classified transactions: deposit / pending / transfer / withdraw"),
        (name = "anonymity-set", description = "Withdrawal anonymity sets: how many withdrawals published each denomination"),
        (name = "pool-notes", description = "Per-chain commitment-tree occupancy"),
    )
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every route the router serves must appear in the spec.
    ///
    /// `utoipa` collects paths from an attribute list that the compiler does not
    /// check against the router, so a new endpoint can ship working and
    /// undocumented. This is the only thing that notices.
    #[test]
    fn the_spec_documents_every_endpoint() {
        let spec = ApiDoc::openapi();
        let paths: Vec<&str> = spec.paths.paths.keys().map(String::as_str).collect();
        for path in [
            "/health",
            "/v1/assets",
            "/v1/tree-advances",
            "/v1/tx-counts",
            "/v1/chain-flows-24h",
            "/v1/asset-flows",
            "/v1/locked",
            "/v1/transactions",
            "/v1/tx-kinds",
            "/v1/anonymity-set",
            "/v1/pool-notes",
        ] {
            assert!(
                paths.contains(&path),
                "{path} missing from the spec: {paths:?}"
            );
        }
    }
}
