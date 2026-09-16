//! Two reads per asset, for a chain without Multicall3.

use super::{AssetRead, HttpMaspYieldReader, IMaspYield, YieldState};
use crate::domain::error::ProtocolIndexerError;
use alloy::providers::Provider;
use chain_types::abi::IYieldVenue;
use tracing::warn;

impl HttpMaspYieldReader {
    /// Two reads per asset plus one for the head, for a chain without
    /// Multicall3. Each asset's pair is joined; the assets run concurrently.
    pub(super) async fn round_per_asset(
        &self,
        reads: &[AssetRead],
    ) -> Result<Vec<Option<YieldState>>, ProtocolIndexerError> {
        let block_number = self
            .inner
            .get_block_number()
            .await
            .map_err(|e| ProtocolIndexerError::Rpc(format!("get_block_number(): {e}")))?;

        Ok(
            futures::future::join_all(reads.iter().map(|read| async move {
                match self.one(*read, block_number).await {
                    Ok(state) => Some(state),
                    Err(e) => {
                        warn!(asset_id = read.id, error = %e, "yield state read failed");
                        None
                    }
                }
            }))
            .await,
        )
    }

    async fn one(
        &self,
        read: AssetRead,
        block_number: u64,
    ) -> Result<YieldState, ProtocolIndexerError> {
        let (r, venue_assets) = tokio::try_join!(
            async {
                IMaspYield::new(read.pool, self.inner.clone())
                    .yieldState(read.id)
                    .call()
                    .await
                    .map_err(|e| {
                        ProtocolIndexerError::Rpc(format!(
                            "{}.yieldState({}): {e}",
                            read.pool, read.id
                        ))
                    })
            },
            async {
                IYieldVenue::new(read.venue, self.inner.clone())
                    .totalAssets()
                    .call()
                    .await
                    .map(|v| v._0)
                    .map_err(|e| {
                        ProtocolIndexerError::Rpc(format!("{}.totalAssets(): {e}", read.venue))
                    })
            },
        )?;

        Ok(Self::state_of(r, venue_assets, block_number))
    }
}
