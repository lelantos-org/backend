// Submits a single MASP.transact() call per chain. Sequential nonces are
// implicit because the pipeline mutex serializes per-chain submissions.
//
// The alloy `ProviderBuilder` output is deeply generic and resists `dyn
// Provider` boxing, so the provider itself is still rebuilt per submission.
// Everything expensive underneath it is not: the signer is parsed once here,
// and the connection pool lives in `RpcEndpoint`.

use crate::adapters::rpc::RpcEndpoint;
use crate::domain::error::{AppError, AppResult};
use alloy::network::EthereumWallet;
use alloy::primitives::{Address, B256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use std::str::FromStr;
use std::time::Duration;
use tracing::{error, info, instrument, warn};

pub struct Submitter {
    pub chain_id: i64,
    pub pool_address: Address,
    pub signer_address: Address,
    pub receipt_timeout_s: u64,
    pub receipt_poll_interval_ms: u64,
    wallet: EthereumWallet,
    rpc: RpcEndpoint,
}

#[derive(Debug, Clone)]
pub struct SubmissionReceipt {
    pub tx_hash: B256,
    /// Mined-in block number. Some chains omit this in pre-finality receipts;
    /// the relayer waits for 1 confirmation so it should always be present.
    pub block_number: i64,
    /// Gas units used by the executed tx (post-EIP-1559 receipt field).
    pub gas_used: u64,
}

impl Submitter {
    pub fn new(
        chain_id: i64,
        rpc: RpcEndpoint,
        signer_key_hex: &str,
        pool_address_hex: &str,
        receipt_timeout_s: u64,
        receipt_poll_interval_ms: u64,
    ) -> AppResult<Self> {
        let signer = PrivateKeySigner::from_str(signer_key_hex)
            .map_err(|e| AppError::Internal(format!("signer key: {}", e)))?;
        let signer_address = signer.address();
        let pool_address = Address::from_str(pool_address_hex)
            .map_err(|e| AppError::Internal(format!("pool addr: {}", e)))?;
        Ok(Self {
            chain_id,
            pool_address,
            signer_address,
            receipt_timeout_s,
            receipt_poll_interval_ms,
            wallet: EthereumWallet::from(signer),
            rpc,
        })
    }

    /// Submit ABI-encoded calldata to the MASP pool. Awaits 1 confirmation.
    /// Pipeline picks the encoding (`flushBatch` / `transfer` / `withdraw` /
    /// `withdrawNative`) and passes the bytes here, keeping this layer
    /// call-shape-agnostic. None of the supported entry points are payable.
    ///
    /// Failure modes are deliberately distinct, because the caller's tree
    /// mirror rollback is only sound for some of them:
    ///   - [`AppError::Rpc`] — the node refused the broadcast; nothing landed.
    ///   - [`AppError::Reverted`] — mined, but reverted; no leaves inserted.
    ///   - [`AppError::SubmitUnknown`] — broadcast accepted, no receipt within
    ///     the timeout. It may still mine. Do not roll back.
    #[instrument(
        skip_all,
        fields(chain_id = self.chain_id, pool = %self.pool_address, calldata_len = data.len()),
    )]
    pub async fn submit(&self, data: Vec<u8>) -> AppResult<SubmissionReceipt> {
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(self.wallet.clone())
            .on_client(self.rpc.client());
        provider
            .client()
            .set_poll_interval(Duration::from_millis(self.receipt_poll_interval_ms));

        let tx = alloy::rpc::types::TransactionRequest::default()
            .to(self.pool_address)
            .input(data.into());

        // eth_call probe surfaces revert data that `send_transaction` would
        // otherwise hide. Only the revert path is logged; success is implied
        // by the subsequent send_transaction path.
        if let Err(e) = provider.call(&tx.clone().from(self.signer_address)).await {
            warn!(error = %e, "eth_call probe reverted; send_transaction will likely fail");
        }
        let pending = provider
            .send_transaction(tx)
            .await
            .map_err(|e| AppError::Rpc(format!("send_transaction: {}", e)))?
            .with_required_confirmations(1)
            .with_timeout(Some(Duration::from_secs(self.receipt_timeout_s)));
        let tx_hash = *pending.tx_hash();
        info!(%tx_hash, "tx submitted, awaiting receipt");
        let receipt = pending.get_receipt().await.map_err(|e| {
            AppError::SubmitUnknown(format!("tx {} broadcast, no receipt: {}", tx_hash, e))
        })?;
        if !receipt.status() {
            error!(tx_hash = %receipt.transaction_hash, "tx reverted on-chain");
            return Err(AppError::Reverted(format!(
                "tx {} reverted",
                receipt.transaction_hash
            )));
        }
        // A receipt without a block number still means the tx executed, so
        // this is an unknown-outcome failure, not a clean one.
        let block_number = receipt.block_number.ok_or_else(|| {
            AppError::SubmitUnknown(format!("tx {} receipt missing block_number", tx_hash))
        })? as i64;
        info!(
            tx_hash = %receipt.transaction_hash,
            block = block_number,
            gas_used = receipt.gas_used,
            "tx confirmed"
        );
        Ok(SubmissionReceipt {
            tx_hash: receipt.transaction_hash,
            block_number,
            gas_used: receipt.gas_used as u64,
        })
    }
}
