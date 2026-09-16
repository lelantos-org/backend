//! The MASP pool's view calls: the few reads the relayer makes of the pool's
//! own state.
//!
//! One reader per chain, shared by the boot checks, the tree mirror's resync, the
//! batcher and the flush pre-flight, so the binding and the error framing each
//! read carries are declared once.

use crate::adapters::abi::IMasp;
use crate::adapters::rpc::{HttpTransport, RpcEndpoint};
use crate::domain::error::{AppError, AppResult};
use alloy::primitives::{Address, B256, U256};
use alloy::providers::{ProviderBuilder, RootProvider};
use alloy::rpc::types::BlockId;
use crypto::tree::Field;
use futures::future::join_all;

type Masp = IMasp::IMaspInstance<HttpTransport, RootProvider<HttpTransport>>;

#[derive(Clone)]
pub struct MaspReader {
    masp: Masp,
}

impl MaspReader {
    pub fn new(rpc: &RpcEndpoint, pool_address: Address) -> Self {
        let provider = ProviderBuilder::new().on_client(rpc.client());
        Self {
            masp: IMasp::new(pool_address, provider),
        }
    }

    pub fn address(&self) -> Address {
        *self.masp.address()
    }

    /// `currentRoot()`, at `block` when given so a later transaction cannot move
    /// it, or at the latest block.
    pub async fn current_root(&self, block: Option<u64>) -> AppResult<Field> {
        let call = self.masp.currentRoot();
        let call = match block {
            Some(n) => call.block(BlockId::number(n)),
            None => call,
        };
        let root = call
            .call()
            .await
            .map_err(|e| AppError::Rpc(format!("currentRoot: {e}")))?
            ._0;
        Ok(root.0)
    }

    /// `rootIndex()` and the root in that ring slot.
    ///
    /// Read as `roots(rootIndex())` rather than `currentRoot()`, so the position
    /// and the root always belong together: a slot keeps its root until 64 more
    /// advances, so an advance landing between the two reads cannot pair a root
    /// with the wrong slot.
    pub async fn newest_ring_slot(&self) -> AppResult<(u32, Field)> {
        let ring_index = self
            .masp
            .rootIndex()
            .call()
            .await
            .map_err(|e| AppError::Rpc(format!("rootIndex: {e}")))?
            ._0;
        let root = self
            .masp
            .roots(U256::from(ring_index))
            .call()
            .await
            .map_err(|e| AppError::Rpc(format!("roots({ring_index}): {e}")))?
            ._0;
        Ok((ring_index, root.0))
    }

    /// `[SPEND_VERIFIER(), TREE_UPDATE_BATCH_VERIFIER()]`, the two addresses a dry
    /// run replaces with an always-accepting stub.
    pub async fn verifiers(&self) -> AppResult<[Address; 2]> {
        let spend = self
            .masp
            .SPEND_VERIFIER()
            .call()
            .await
            .map_err(|e| AppError::Rpc(format!("SPEND_VERIFIER(): {e}")))?
            ._0;
        let tree = self
            .masp
            .TREE_UPDATE_BATCH_VERIFIER()
            .call()
            .await
            .map_err(|e| AppError::Rpc(format!("TREE_UPDATE_BATCH_VERIFIER(): {e}")))?
            ._0;
        Ok([spend, tree])
    }

    /// `escrowed(id)` for each id, one `eth_call` per id issued concurrently. Zero
    /// means no pending deposit; anything else is the submit-time digest that
    /// `flushBatch` re-derives and compares. A flush batch holds at most
    /// `MAX_DEPOSITS_PER_BATCH` deposits.
    ///
    /// The result is positional: `[i]` is the slot for `ids[i]` and the length
    /// always matches `ids`. Callers pair deposits to slots by index, which keeps
    /// a deposit from being judged against another's escrow.
    ///
    /// Any transport failure fails the whole read, so a deposit is never judged
    /// unflushable because the node was unreachable.
    pub async fn escrowed(&self, ids: &[u64]) -> AppResult<Vec<B256>> {
        join_all(ids.iter().map(|id| async move {
            self.masp
                .escrowed(U256::from(*id))
                .call()
                .await
                .map(|r| r.digest)
                .map_err(|e| AppError::Rpc(format!("escrowed({id}): {e}")))
        }))
        .await
        .into_iter()
        .collect()
    }
}
