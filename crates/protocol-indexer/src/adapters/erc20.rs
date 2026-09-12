//! ERC20 metadata reads.
//!
//! `AssetRegistered` carries only `(assetId, token, scale)`, and `scale` is a
//! circuit capacity parameter rather than a decimals normalizer; see
//! `contracts/script/Deploy.s.sol`. Rendering a human-readable amount therefore
//! requires the token's own `decimals()`, which only the chain can answer.

use crate::domain::error::ProtocolIndexerError;
use alloy::primitives::Address;
use alloy::providers::{ProviderBuilder, RootProvider};
use async_trait::async_trait;
use chain_types::abi::{IERC20Metadata, IYieldVenue};
use chain_types::rpc::{HttpTransport, RpcEndpoint, RpcTimeouts};
use std::sync::Arc;

#[async_trait]
pub trait TokenMetadata: Send + Sync {
    async fn decimals(&self, token: Address) -> Result<u8, ProtocolIndexerError>;
    /// The token's own label.
    ///
    /// Fallible beyond transport errors: `symbol()` is optional in ERC-20, and
    /// some early tokens return `bytes32` rather than `string`, which does not
    /// decode here. The caller leaves the column NULL and retries.
    async fn symbol(&self, token: Address) -> Result<String, ProtocolIndexerError>;
    /// The label of the ERC-4626 vault behind a yield venue: `VAULT()` on the
    /// venue, then `name()` on that vault.
    ///
    /// Fallible for the same reason as [`Self::symbol`]: `name()` is optional
    /// metadata. The caller leaves the column NULL and retries.
    async fn vault_name(&self, venue: Address) -> Result<String, ProtocolIndexerError>;
}

/// Deadline for one metadata call.
///
/// `decimals()`/`symbol()` are single reads at the head, so this only has to
/// exceed a slow node rather than a slow query. Bounded at all because the
/// sweep runs inside a tick: an untimed call against a hung node would park the
/// chain's consume loop indefinitely.
const TIMEOUTS: RpcTimeouts = RpcTimeouts::request(10);

pub type DynTokenMetadata = Arc<dyn TokenMetadata>;

pub struct HttpTokenMetadata {
    inner: RootProvider<HttpTransport>,
}

impl HttpTokenMetadata {
    pub fn build(rpc_url: &str) -> Result<Arc<Self>, ProtocolIndexerError> {
        let rpc = RpcEndpoint::new(rpc_url, TIMEOUTS)
            .map_err(|e| ProtocolIndexerError::Config(format!("rpc_url: {e}")))?;
        Ok(Arc::new(Self {
            inner: ProviderBuilder::new().on_client(rpc.client()),
        }))
    }
}

#[async_trait]
impl TokenMetadata for HttpTokenMetadata {
    async fn decimals(&self, token: Address) -> Result<u8, ProtocolIndexerError> {
        IERC20Metadata::new(token, self.inner.clone())
            .decimals()
            .call()
            .await
            .map(|r| r._0)
            .map_err(|e| ProtocolIndexerError::Rpc(format!("{token}.decimals(): {e}")))
    }

    async fn symbol(&self, token: Address) -> Result<String, ProtocolIndexerError> {
        IERC20Metadata::new(token, self.inner.clone())
            .symbol()
            .call()
            .await
            .map(|r| r._0)
            .map_err(|e| ProtocolIndexerError::Rpc(format!("{token}.symbol(): {e}")))
    }

    async fn vault_name(&self, venue: Address) -> Result<String, ProtocolIndexerError> {
        let vault = IYieldVenue::new(venue, self.inner.clone())
            .VAULT()
            .call()
            .await
            .map(|r| r._0)
            .map_err(|e| ProtocolIndexerError::Rpc(format!("{venue}.VAULT(): {e}")))?;
        IERC20Metadata::new(vault, self.inner.clone())
            .name()
            .call()
            .await
            .map(|r| r._0)
            .map_err(|e| ProtocolIndexerError::Rpc(format!("{vault}.name(): {e}")))
    }
}
