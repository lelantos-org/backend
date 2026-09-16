//! Path 2: the rate from the venue's vault, the bootstrap before the history fills.

use super::VenueApyWorker;
use crate::adapters::venue::{BlockRef, ShareReadError};
use crate::domain::apy::{annualize_bps, net_of_pool, window_start_block};
use alloy::rpc::types::BlockNumberOrTag;
use asset_registry::{ApyEstimate, AssetRow};
use tracing::debug;

/// Blocks back for the block-time probe. Only sizes the estimate of where the
/// window starts, so it does not have to be exact — see
/// [`crate::domain::apy::window_start_block`].
const PROBE_BLOCKS: u64 = 5_000;

impl VenueApyWorker {
    /// `(head, window start)` as `(block number, unix seconds)`.
    pub(super) async fn window(&self) -> Option<(BlockRef, BlockRef)> {
        let head = self.venue.block(BlockNumberOrTag::Latest).await?;
        let probe_at = head.0.saturating_sub(PROBE_BLOCKS);
        let probe = self.venue.block(BlockNumberOrTag::Number(probe_at)).await?;
        let start = window_start_block(head.0, head.1, probe.0, probe.1)?;
        let target = self.venue.block(BlockNumberOrTag::Number(start)).await?;
        Some((head, target))
    }

    /// One asset's estimate from the venue's vault, or `None` if either reading
    /// failed.
    ///
    /// The bootstrap path: it answers before this deployment has a history of its
    /// own, and needs an RPC serving state a window back. See the module header
    /// for why it is second choice once [`super::recorded::rate`] can answer.
    pub(super) async fn vault_rate(
        &mut self,
        head: u64,
        target: u64,
        elapsed: i64,
        asset: &AssetRow,
    ) -> Option<ApyEstimate> {
        let (now, then) = match self
            .venue
            .share_prices(asset.venue_address()?, head, target)
            .await
        {
            Ok(pair) => pair,
            // Overwhelmingly an RPC without state that far back, which is a
            // property of the endpoint rather than of the asset. Logged once per
            // asset per pass, at debug: on a pruned node this is every asset,
            // every pass, forever.
            Err(ShareReadError::Readings { head_ok, window_ok }) => {
                debug!(
                    chain_id = self.chain_id,
                    asset_id = asset.asset_id(),
                    head_ok,
                    window_ok,
                    "venue apy: vault read failed"
                );
                return None;
            }
            Err(ShareReadError::Vault) => return None,
        };

        let gross = annualize_bps(now, then, elapsed)?;
        Some(ApyEstimate {
            bps: net_of_pool(
                gross,
                asset.perf_bps.unwrap_or(0),
                asset.buffer_bps.unwrap_or(0),
            ),
            window_s: elapsed,
        })
    }
}
