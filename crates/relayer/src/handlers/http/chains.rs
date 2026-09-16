use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::{ChainHealth, ChainsResponse};
use crate::services::tree::{self, field_to_hex};
use axum::Json;
use axum::extract::State;

/// What this relayer reports about itself, per chain.
///
/// Half of the pair a wallet boots from: protocol-webserver's `/v1/chains` says
/// what each chain *is* — its name, browser RPC, explorer, and the contracts the
/// deployment declares — while this says what this one relayer will do on it.
/// The two overlap on `maspAddress` and `treeDepth` deliberately, and a wallet
/// that finds them disagreeing drops the chain rather than proving against a
/// pool this relayer does not write to.
///
/// A relayer serving a chain the registry does not describe still appears here;
/// it is the wallet that decides it has nothing usable to say.
pub async fn chains(State(st): State<AppState>) -> AppResult<Json<ChainsResponse>> {
    let mut chains = Vec::with_capacity(st.spend_pipelines.len());
    for (chain_id, pipeline) in st.spend_pipelines.iter() {
        // Read the published snapshot rather than the mirror itself. The batcher
        // holds the mirror from reserve through prove and confirmation, so locking
        // it here would park the boot endpoint behind whatever bundle is in flight.
        let snapshot = &pipeline.snapshot;
        // Only the fee terms need the catalog now — `tokens` moved to
        // protocol-webserver's `/v1/assets` — so a relayer that charges nothing
        // does not read the asset table to answer this at all.
        let shielded_fee = match pipeline.shielded_fee.as_ref() {
            // Through the shared registry rather than the pool: every wallet
            // boots from this route and the relayer holds four connections.
            Some(f) => Some(f.terms(&st.assets.for_chain(*chain_id).await?)),
            None => None,
        };
        chains.push(ChainHealth {
            chain_id: *chain_id,
            committed_count: snapshot.leaf_count() as i64,
            current_root_hex: field_to_hex(&snapshot.root()),
            masp_address: pipeline.pool_address.to_checksum(None),
            desynced: snapshot.is_desynced(),
            // The relayer's Bundler, not its signing key: the Bundler is the pool's
            // caller, so it is what a spend proof binds as `relayer` and a swap as
            // `payer`.
            relayer_address: pipeline.bundler_address.to_checksum(None),
            // An account, unlike `relayer_address`: somewhere a swap's cancelled
            // escrow can be refunded to by a wallet with no EVM account.
            refund_address: pipeline.refund_address.to_checksum(None),
            // The depth this binary mirrors, not one it was told. A configured
            // value could disagree with the tree actually being verified against,
            // which is precisely what the wallet's cross-check exists to catch.
            tree_depth: tree::DEPTH as u32,
            shielded_fee,
        });
    }
    chains.sort_by_key(|c| c.chain_id);
    Ok(Json(ChainsResponse { chains }))
}
