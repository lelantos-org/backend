use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::{ChainConfigOut, ChainHealth, ChainsResponse, TokenOut};
use crate::services::tree::field_to_hex;
use axum::Json;
use axum::extract::State;

/// Health, configuration, and the registered assets for every chain.
///
/// The registry a client boots from. The relayer is the only service that already
/// enumerates every chain, so publishing the wallet-facing half of its config
/// here, together with the asset list explorer-indexer maintains, lets a
/// deployment add a chain without rebuilding any frontend and removes a per-token
/// RPC read from every client.
///
/// A chain the operator has not described still appears, carrying only its live
/// readings; omitted fields leave the client on its own defaults.
pub async fn chains(State(st): State<AppState>) -> AppResult<Json<ChainsResponse>> {
    let mut chains = Vec::with_capacity(st.spend_pipelines.len());
    for (chain_id, pipeline) in st.spend_pipelines.iter() {
        // Through the shared registry rather than the pool: every wallet boots
        // from this route and the relayer holds four connections.
        let assets = st.assets.for_chain(*chain_id).await?;
        // The estimate comes from the worker's cache, never from a read of its
        // own: `/chains` is what every wallet boots from and polls, and two
        // archive calls per yield asset per request would put a week-long
        // measurement on the critical path of every page load.
        let mut tokens = Vec::with_capacity(assets.len());
        for a in assets.iter() {
            // Only a live venue can have an entry, and the response drops the
            // rate for a halted one anyway — so a plain-custody asset is a
            // guaranteed miss, and on a chain of twenty assets with three venues
            // that is seventeen lookups and seventeen await points saved on the
            // route every wallet polls.
            let apy = match a.venue.is_some() && !a.halted.unwrap_or(false) {
                true => st.venue_apy.get(&(*chain_id, a.asset_id())).await,
                false => None,
            };
            tokens.push(TokenOut::new(a, apy));
        }

        // Read the published snapshot rather than the mirror itself. The mirror
        // mutex is held from reserve through prove and confirmation, so locking it
        // here would park the boot endpoint behind whatever submission is in
        // flight.
        let snapshot = &pipeline.snapshot;
        chains.push(ChainHealth {
            chain_id: *chain_id,
            committed_count: snapshot.leaf_count() as i64,
            current_root_hex: field_to_hex(&snapshot.root()),
            masp_address: pipeline.submitter.pool_address.to_checksum(None),
            desynced: snapshot.is_desynced(),
            relayer_address: pipeline.submitter.signer_address.to_checksum(None),
            config: st
                .descriptors
                .get(chain_id)
                .map(ChainConfigOut::from)
                .unwrap_or_default(),
            tokens,
            shielded_fee: pipeline.shielded_fee.as_ref().map(|f| f.terms(&assets)),
        });
    }
    chains.sort_by_key(|c| c.chain_id);
    Ok(Json(ChainsResponse { chains }))
}
