//! The chain reads behind the venue-APY measurement.
//!
//! Everything in this file is one JSON-RPC call and its failure mode; what the
//! readings then mean is [`crate::domain::apy`], and what is done with them is
//! [`crate::services::venue_apy`]. The split is what keeps the arithmetic
//! testable without a node and the orchestration readable without alloy.

use crate::adapters::rpc::{HttpTransport, RpcEndpoint};
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::types::BlockNumberOrTag;
use chain_types::abi::{IERC4626, IYieldVenue};
use std::collections::HashMap;
use tracing::warn;

/// A block's number and its timestamp, in unix seconds.
pub type BlockRef = (u64, i64);

/// Why a pair of share-price readings could not be taken.
///
/// Two cases rather than `None`, because they say different things about the
/// deployment: one is a venue that will not describe itself, the other an
/// endpoint without the state being asked for.
#[derive(Debug, Clone, Copy)]
pub enum ShareReadError {
    /// The venue would not name its vault, or the vault its decimals.
    Vault,
    /// The archive reads failed. Which of the two did is the useful part: both
    /// failing is an endpoint without state that far back, one failing is a
    /// block that has gone away.
    Readings { head_ok: bool, window_ok: bool },
}

/// One chain's read-only view of its yield venues.
///
/// Built on [`RpcEndpoint`] rather than a bare provider, so every call here
/// carries the request deadline: an untimed call against a hung node would stall
/// a chain's measurement forever while holding its election lock.
pub struct VenueReader {
    chain_id: i64,
    provider: RootProvider<HttpTransport>,
    /// `venue -> (vault, share decimals)`, resolved once. A venue is pinned to
    /// its vault at construction and cannot be re-pointed, and a vault's decimals
    /// are immutable, so neither ever needs invalidating.
    vaults: HashMap<Address, (Address, u8)>,
}

impl VenueReader {
    pub fn new(chain_id: i64, rpc: &RpcEndpoint) -> Self {
        Self {
            chain_id,
            provider: ProviderBuilder::new().on_client(rpc.client()),
            vaults: HashMap::new(),
        }
    }

    /// One block's number and timestamp, or `None` if it could not be read.
    pub async fn block(&self, at: BlockNumberOrTag) -> Option<BlockRef> {
        /// Headers only. The worker reads a number and a timestamp; pulling
        /// every transaction body of a full block to get them would be the
        /// bulk of the response.
        const HYDRATE_TXS: bool = false;

        match self.provider.get_block_by_number(at, HYDRATE_TXS).await {
            Ok(Some(b)) => Some((b.header.number, b.header.timestamp as i64)),
            Ok(None) => None,
            Err(e) => {
                warn!(chain_id = self.chain_id, error = %e, "venue apy: block read failed");
                None
            }
        }
    }

    /// A venue's vault share price at `head` and at `target`.
    ///
    /// Both are archive reads, and the two are independent, so they are issued
    /// together: their latency is the bulk of a measurement pass.
    pub async fn share_prices(
        &mut self,
        venue: Address,
        head: u64,
        target: u64,
    ) -> Result<(U256, U256), ShareReadError> {
        let (vault, decimals) = self.vault_of(venue).await.ok_or(ShareReadError::Vault)?;
        let vault = IERC4626::new(vault, &self.provider);

        // One whole share, times a million. `convertToAssets` answers in the
        // asset's own decimals, which for a 6-decimal token leaves a week of
        // growth in the last three digits; the multiplier buys six more, and a
        // ratio of two readings of the same probe is unaffected by its size.
        let probe = U256::from(10u64)
            .checked_pow(U256::from(u32::from(decimals) + 6))
            .ok_or(ShareReadError::Vault)?;

        // Bound first — the builders are temporaries the futures borrow from.
        let at_head = vault.convertToAssets(probe).block(head.into());
        let at_target = vault.convertToAssets(probe).block(target.into());
        let (now, then) = tokio::join!(at_head.call(), at_target.call());
        match (now, then) {
            (Ok(n), Ok(t)) => Ok((n._0, t._0)),
            (n, t) => Err(ShareReadError::Readings {
                head_ok: n.is_ok(),
                window_ok: t.is_ok(),
            }),
        }
    }

    /// The vault behind a venue and its share decimals, read once and
    /// remembered. Both immutable on chain, so re-reading them every pass would
    /// spend a round trip to be told the same thing.
    async fn vault_of(&mut self, venue: Address) -> Option<(Address, u8)> {
        if let Some(v) = self.vaults.get(&venue) {
            return Some(*v);
        }
        let vault = IYieldVenue::new(venue, &self.provider)
            .VAULT()
            .call()
            .await
            .ok()?
            ._0;
        let decimals = IERC4626::new(vault, &self.provider)
            .decimals()
            .call()
            .await
            .ok()?
            ._0;
        self.vaults.insert(venue, (vault, decimals));
        Some((vault, decimals))
    }
}
