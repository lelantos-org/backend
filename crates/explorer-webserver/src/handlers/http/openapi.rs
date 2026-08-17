use crate::domain::responses::{
    AssetOut, ChainFlowOut, ChainLockedOut, CountPoint, FlowPoint, KindCounts, LockedAssetOut,
    TreeAdvanceOut, TxKind, TxOut,
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
        KindCounts
    )),
    tags(
        (name = "health", description = "Health and build info"),
        (name = "assets", description = "Assets"),
        (name = "tree-advances", description = "Tree advances"),
        (name = "asset-flows", description = "Per-token deposit/withdraw flows"),
        (name = "locked", description = "Escrowed balances per chain: all-time deposits minus withdrawals"),
        (name = "transactions", description = "Classified transactions: deposit / pending / transfer / withdraw"),
    )
)]
pub struct ApiDoc;
