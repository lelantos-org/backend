//! Reads `MASP.escrowed(id)`, the only deposit state the contract keeps. Zero
//! means no pending deposit; anything else is the submit-time digest that
//! `flushBatch` re-derives and compares.
//!
//! The flush pipeline uses this to drop deposits that cannot land before the
//! prover runs; see `pipeline::flush::FlushPipeline::preflight`.

use crate::adapters::abi::IMasp;
use crate::adapters::rpc::RpcEndpoint;
use crate::domain::error::{AppError, AppResult};
use alloy::primitives::{Address, B256, U256};
use alloy::providers::ProviderBuilder;
use futures::future::join_all;
use std::str::FromStr;

pub struct EscrowReader {
    rpc: RpcEndpoint,
    pool_address: Address,
}

impl EscrowReader {
    pub fn new(rpc: RpcEndpoint, pool_address_hex: &str) -> AppResult<Self> {
        let pool_address = Address::from_str(pool_address_hex)
            .map_err(|e| AppError::Internal(format!("pool addr: {}", e)))?;
        Ok(Self { rpc, pool_address })
    }

    pub fn pool_address(&self) -> Address {
        self.pool_address
    }

    /// The stored digest for each id, one `eth_call` per id issued concurrently.
    /// A flush batch holds at most `MAX_DEPOSITS_PER_BATCH` deposits.
    ///
    /// The result is positional: `[i]` is the slot for `ids[i]` and the length
    /// always matches `ids`. Callers pair deposits to slots by index, which keeps
    /// a deposit from being judged against another's escrow.
    ///
    /// Any transport failure fails the whole read, so a deposit is never judged
    /// unflushable because the node was unreachable.
    pub async fn digests(&self, ids: &[u64]) -> AppResult<Vec<B256>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let provider = ProviderBuilder::new().on_client(self.rpc.client());
        let masp = IMasp::new(self.pool_address, provider);
        join_all(ids.iter().map(|id| {
            let masp = &masp;
            async move {
                masp.escrowed(U256::from(*id))
                    .call()
                    .await
                    .map(|r| r.digest)
                    .map_err(|e| AppError::Rpc(format!("escrowed({id}): {e}")))
            }
        }))
        .await
        .into_iter()
        .collect()
    }
}
