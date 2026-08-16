//! ERC20 metadata reads.
//!
//! `AssetRegistered` carries `(assetId, token, scale)` and nothing else, but
//! `scale` is a circuit capacity parameter, not a decimals normalizer — see
//! `contracts/script/Deploy.s.sol`. Rendering a human amount therefore needs
//! the token's own `decimals()`, which only the chain can answer.

use crate::error::ExplorerIndexerError;
use alloy::primitives::Address;
use alloy::providers::{ProviderBuilder, RootProvider};
use alloy::sol;
use alloy::transports::http::reqwest::Url;
use alloy::transports::http::{Client, Http};
use async_trait::async_trait;
use std::sync::Arc;

sol! {
    #[sol(rpc)]
    interface IERC20Metadata {
        function decimals() external view returns (uint8);
        function symbol() external view returns (string);
    }
}

#[async_trait]
pub trait TokenMetadata: Send + Sync {
    async fn decimals(&self, token: Address) -> Result<u8, ExplorerIndexerError>;
    /// The token's own label.
    ///
    /// Fallible for more than transport reasons: `symbol()` is optional in
    /// ERC-20, and a handful of early tokens return `bytes32` rather than
    /// `string`, which does not decode here. Either way the caller leaves the
    /// column NULL and retries, rather than inventing a name.
    async fn symbol(&self, token: Address) -> Result<String, ExplorerIndexerError>;
}

pub type DynTokenMetadata = Arc<dyn TokenMetadata>;

pub struct HttpTokenMetadata {
    inner: RootProvider<Http<Client>>,
}

impl HttpTokenMetadata {
    pub fn build(rpc_url: &str) -> Result<Arc<Self>, ExplorerIndexerError> {
        let url: Url = rpc_url
            .parse()
            .map_err(|e| ExplorerIndexerError::Config(format!("rpc_url: {e}")))?;
        Ok(Arc::new(Self {
            inner: ProviderBuilder::new().on_http(url),
        }))
    }
}

#[async_trait]
impl TokenMetadata for HttpTokenMetadata {
    async fn decimals(&self, token: Address) -> Result<u8, ExplorerIndexerError> {
        IERC20Metadata::new(token, self.inner.clone())
            .decimals()
            .call()
            .await
            .map(|r| r._0)
            .map_err(|e| ExplorerIndexerError::Rpc(format!("{token}.decimals(): {e}")))
    }

    async fn symbol(&self, token: Address) -> Result<String, ExplorerIndexerError> {
        IERC20Metadata::new(token, self.inner.clone())
            .symbol()
            .call()
            .await
            .map(|r| r._0)
            .map_err(|e| ExplorerIndexerError::Rpc(format!("{token}.symbol(): {e}")))
    }
}
