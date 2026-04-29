// Submits a single MASP.transact() call per chain. Sequential nonces are
// implicit because the pipeline mutex serializes per-chain submissions.
//
// v1: build provider per submission (cheap; alloy ProviderBuilder generates
// deeply-generic types that resist `dyn Provider` boxing). Defer connection
// pooling / persistent provider once we settle on a fixed type alias.

use crate::domain::error::{AppError, AppResult};
use alloy::primitives::{Address, B256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use std::str::FromStr;
use std::time::Duration;
use tracing::{error, info, instrument, warn};

pub struct Submitter {
    pub chain_id: i64,
    pub rpc_url: String,
    pub pool_address: Address,
    pub signer_key_hex: String,
    pub signer_address: Address,
    pub receipt_timeout_s: u64,
    pub receipt_poll_interval_ms: u64,
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
        rpc_url: &str,
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
            rpc_url: rpc_url.to_string(),
            pool_address,
            signer_key_hex: signer_key_hex.to_string(),
            signer_address,
            receipt_timeout_s,
            receipt_poll_interval_ms,
        })
    }

    /// Submit ABI-encoded calldata to the MASP pool. Awaits 1 confirmation.
    /// Pipeline picks the encoding (`flushBatch` / `transfer` / `withdraw` /
    /// `withdrawNative`) and passes the bytes here, keeping this layer
    /// call-shape-agnostic. None of the supported entry points are payable.
    #[instrument(
        skip_all,
        fields(chain_id = self.chain_id, pool = %self.pool_address, calldata_len = data.len()),
    )]
    pub async fn submit(&self, data: Vec<u8>) -> AppResult<SubmissionReceipt> {
        let signer = PrivateKeySigner::from_str(&self.signer_key_hex)
            .map_err(|e| AppError::Internal(format!("signer key: {}", e)))?;
        let url: alloy::transports::http::reqwest::Url = self
            .rpc_url
            .parse()
            .map_err(|e: url::ParseError| AppError::Internal(format!("rpc url: {}", e)))?;
        let wallet = alloy::network::EthereumWallet::from(signer);
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(wallet)
            .on_http(url);
        provider
            .client()
            .set_poll_interval(Duration::from_millis(self.receipt_poll_interval_ms));

        let tx = alloy::rpc::types::TransactionRequest::default()
            .to(self.pool_address)
            .input(data.clone().into());

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
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|e| AppError::Rpc(format!("receipt: {}", e)))?;
        if !receipt.status() {
            error!(tx_hash = %receipt.transaction_hash, "tx reverted on-chain");
            return Err(AppError::Reverted(format!(
                "tx {} reverted",
                receipt.transaction_hash
            )));
        }
        let block_number = receipt
            .block_number
            .ok_or_else(|| AppError::Rpc("receipt missing block_number".into()))?
            as i64;
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
