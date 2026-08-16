use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::{ChainConfigOut, ChainHealth, ChainsResponse, TokenOut};
use crate::repositories::assets;
use crate::services::tree::field_to_hex;
use axum::Json;
use axum::extract::State;

/// Health, configuration, and the registered assets for every chain.
///
/// This is the registry a client boots from: the relayer is the only service
/// that already enumerates every chain, so publishing the wallet-facing half
/// of its config here — plus the asset list explorer-indexer maintains — is
/// what lets a deployment add a chain without rebuilding any frontend, and
/// what removes a per-token RPC read from every client.
///
/// A chain the operator has not described yet still appears, carrying only
/// its live readings; omitted fields leave the client on its own defaults
/// rather than on a guess.
pub async fn chains(State(st): State<AppState>) -> AppResult<Json<ChainsResponse>> {
    let mut chains = Vec::with_capacity(st.spend_pipelines.len());
    for (chain_id, pipeline) in st.spend_pipelines.iter() {
        // Read before taking the mirror lock: the query is unrelated to the
        // tree, and holding the lock across it would serialise every spend on
        // this chain behind a database round trip.
        let tokens: Vec<TokenOut> = assets::list_for_chain(&st.pool, *chain_id)
            .await?
            .into_iter()
            .map(TokenOut::from)
            .collect();

        let mirror = pipeline.mirror.lock().await;
        let root = mirror.current_root()?;
        chains.push(ChainHealth {
            chain_id: *chain_id,
            committed_count: mirror.committed_count() as i64,
            current_root_hex: field_to_hex(&root),
            masp_address: pipeline.submitter.pool_address.to_checksum(None),
            desynced: mirror.is_desynced(),
            relayer_address: pipeline.submitter.signer_address.to_checksum(None),
            config: st
                .descriptors
                .get(chain_id)
                .map(ChainConfigOut::from)
                .unwrap_or_default(),
            tokens,
        });
    }
    chains.sort_by_key(|c| c.chain_id);
    Ok(Json(ChainsResponse { chains }))
}
