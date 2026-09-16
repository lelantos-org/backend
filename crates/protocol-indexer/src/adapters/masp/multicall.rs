//! One `eth_call` per round, through Multicall3.

use super::{AssetRead, HttpMaspYieldReader, IMaspYield, YieldState};
use crate::domain::error::ProtocolIndexerError;
use alloy::providers::Provider;
use alloy::sol_types::SolCall;
use chain_types::abi::{IMulticall3, IYieldVenue, MULTICALL3};
use tracing::{info, warn};

impl HttpMaspYieldReader {
    /// Whether this chain carries Multicall3, probed once.
    pub(super) async fn has_multicall(&self) -> bool {
        *self
            .multicall
            .get_or_try_init(|| async {
                let present = !self.inner.get_code_at(MULTICALL3).await?.is_empty();
                // Both outcomes, once: this is the fact that explains the
                // service's whole request count, and inferring it from a node's
                // logs is not something anyone should have to do.
                if present {
                    info!(address = %MULTICALL3, "yield rounds batch through Multicall3");
                } else {
                    info!(address = %MULTICALL3, "no Multicall3; yield rounds read per asset");
                }
                Ok::<bool, alloy::transports::RpcError<alloy::transports::TransportErrorKind>>(
                    present,
                )
            })
            .await
            .inspect_err(|e| warn!(error = %e, "Multicall3 probe failed; reading per asset"))
            .unwrap_or(&false)
    }

    /// One `eth_call`: the head plus every asset's two reads.
    ///
    /// `allowFailure` per call, so a venue that reverts costs its own asset and
    /// nothing else — the same isolation the per-asset path gets from handling
    /// each `Result` separately.
    pub(super) async fn round_batched(
        &self,
        reads: &[AssetRead],
    ) -> Result<Vec<Option<YieldState>>, ProtocolIndexerError> {
        let mut calls = Vec::with_capacity(reads.len() * 2 + 1);
        calls.push(IMulticall3::Call3 {
            target: MULTICALL3,
            allowFailure: false,
            callData: IMulticall3::getBlockNumberCall {}.abi_encode().into(),
        });
        for r in reads {
            calls.push(IMulticall3::Call3 {
                target: r.pool,
                allowFailure: true,
                callData: IMaspYield::yieldStateCall { id: r.id }.abi_encode().into(),
            });
            calls.push(IMulticall3::Call3 {
                target: r.venue,
                allowFailure: true,
                callData: IYieldVenue::totalAssetsCall {}.abi_encode().into(),
            });
        }

        let out = IMulticall3::new(MULTICALL3, self.inner.clone())
            .aggregate3(calls)
            .call()
            .await
            .map_err(|e| ProtocolIndexerError::Rpc(format!("multicall3.aggregate3: {e}")))?
            .returnData;

        // `allowFailure: false` on the head, so Multicall3 reverts the batch
        // rather than returning it unset — a missing head here means the call
        // failed wholesale, not that one asset did.
        let head = out
            .first()
            .filter(|r| r.success)
            .ok_or_else(|| ProtocolIndexerError::Rpc("multicall3: no block number".into()))?;
        let block_number: u64 =
            IMulticall3::getBlockNumberCall::abi_decode_returns(&head.returnData, false)
                .map_err(|e| ProtocolIndexerError::Rpc(format!("multicall3.getBlockNumber: {e}")))?
                .blockNumber
                .try_into()
                // `U256::to` panics rather than truncating, and a panic here takes
                // the whole poller down silently.
                .map_err(|_| ProtocolIndexerError::Rpc("block number exceeds u64".into()))?;

        // Paired off the tail rather than indexed with `1 + i * 2`: the layout is
        // then stated once, where the calls are pushed, instead of twice.
        let mut pairs = out.get(1..).unwrap_or_default().chunks_exact(2);
        let states = reads
            .iter()
            .map(|read| {
                let [state, assets] = pairs.next()? else {
                    warn!(
                        asset_id = read.id,
                        "multicall3 returned no result for this asset"
                    );
                    return None;
                };
                if !state.success || !assets.success {
                    // Logged here because the per-asset path logs its failures
                    // too; silence on one path only would make a permanently
                    // stale row invisible on whichever chain took it.
                    warn!(
                        asset_id = read.id,
                        venue = %read.venue,
                        yield_state_ok = state.success,
                        total_assets_ok = assets.success,
                        "yield read reverted"
                    );
                    return None;
                }
                let r = IMaspYield::yieldStateCall::abi_decode_returns(&state.returnData, false)
                    .inspect_err(
                        |e| warn!(asset_id = read.id, error = %e, "yieldState decode failed"),
                    )
                    .ok()?;
                let venue_assets =
                    IYieldVenue::totalAssetsCall::abi_decode_returns(&assets.returnData, false)
                        .inspect_err(
                            |e| warn!(venue = %read.venue, error = %e, "totalAssets decode failed"),
                        )
                        .ok()?
                        ._0;
                Some(Self::state_of(r, venue_assets, block_number))
            })
            .collect();

        Ok(states)
    }
}
