// Submits a single MASP.transact() call per chain. Sequential nonces are
// implicit because the pipeline mutex serializes per-chain submissions.
//
// The alloy `ProviderBuilder` output is deeply generic and resists `dyn
// Provider` boxing, so the provider itself is still rebuilt per submission.
// Everything expensive underneath it is not: the signer is parsed once here,
// and the connection pool lives in `RpcEndpoint`.

use crate::adapters::rpc::RpcEndpoint;
use crate::domain::error::{AppError, AppResult, revert_reason};
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
        // otherwise hide behind a gas-estimation failure. Keep the reason: it
        // is the only place the rejecting guard is legible, and the send that
        // follows reports the same rejection as an opaque RPC error.
        let probe_revert = match provider.call(&tx.clone().from(self.signer_address)).await {
            Ok(_) => None,
            Err(e) => {
                warn!(error = %e, "eth_call probe reverted; send_transaction will likely fail");
                Some(e.to_string())
            }
        };
        let pending = provider
            .send_transaction(tx)
            .await
            .map_err(|e| classify_send_failure(probe_revert.as_deref(), &e.to_string()))?
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

/// Decide what a `send_transaction` failure actually was.
///
/// A contract guard that rejects the payload surfaces here as a gas-estimation
/// failure — RPC-shaped, but not an RPC fault. Prefer the probe's message,
/// which states the revert plainly; otherwise sniff the send error for the
/// same marker, since the probe can succeed against a state the transaction is
/// later priced against.
fn classify_send_failure(probe_revert: Option<&str>, send_err: &str) -> AppError {
    let rejected = |detail: &str| {
        revert_reason(detail).map(|reason| AppError::ContractRejected {
            reason,
            detail: detail.to_string(),
        })
    };
    probe_revert
        .and_then(rejected)
        .or_else(|| rejected(send_err))
        .unwrap_or_else(|| AppError::Rpc(format!("send_transaction: {}", send_err)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The secret an RPC URL usually carries.
    const NODE_URL: &str = "https://mainnet.example.com/v3/deadbeefsecretkey";

    fn probe_error() -> String {
        format!(
            "server returned an error response for {NODE_URL}: error code 3: \
             execution reverted: MockSwapRouter02: too little received, data: \"0x08c379a0\""
        )
    }

    #[test]
    fn a_probe_revert_is_a_contract_rejection_the_caller_can_act_on() {
        let err = classify_send_failure(Some(&probe_error()), "gas estimation failed");
        assert!(matches!(err, AppError::ContractRejected { .. }));
        assert!(err.status().is_client_error(), "got {}", err.status());
        assert!(err.client_message().contains("too little received"));
    }

    /// The whole point of the variant: the reason reaches the caller while the
    /// node URL that carried it does not.
    #[test]
    fn the_rejection_reason_reaches_the_caller_but_the_node_url_does_not() {
        let err = classify_send_failure(Some(&probe_error()), "gas estimation failed");
        let msg = err.client_message();
        assert!(!msg.contains("example.com"), "leaked host: {msg}");
        assert!(!msg.contains("deadbeefsecretkey"), "leaked key: {msg}");
    }

    #[test]
    fn a_revert_the_probe_missed_is_still_a_contract_rejection() {
        let err = classify_send_failure(None, &probe_error());
        assert!(matches!(err, AppError::ContractRejected { .. }));
    }

    /// A probe that failed for transport reasons says nothing about the
    /// payload, so it must not be reported as the caller's fault.
    #[test]
    fn a_transport_failure_stays_an_rpc_error() {
        let transport = format!("error sending request for url {NODE_URL}");
        let err = classify_send_failure(Some(&transport), &transport);
        assert!(matches!(err, AppError::Rpc(_)));
        assert_eq!(err.client_message(), "internal error");
    }
}
