// Submits a single MASP.transact() call per chain. Sequential nonces are
// implicit because the pipeline mutex serializes per-chain submissions.
//
// The provider is built once, at construction. Alloy's filler stack is deeply
// generic and resists `dyn Provider` boxing, which is why it used to be rebuilt
// per submission — but the type can be named, and rebuilding it threw away
// `ChainIdFiller`'s cache, so every submission paid an `eth_chainId` round trip
// before the transaction went out. See [`PoolProvider`].

use crate::adapters::rpc::{HttpTransport, RpcEndpoint};
use crate::domain::error::{AppError, AppResult, revert_reason};
use alloy::consensus::TxEnvelope;
use alloy::network::{Ethereum, EthereumWallet};
use alloy::primitives::{Address, B256};
use alloy::providers::fillers::{FillProvider, JoinFill, RecommendedFillers, WalletFiller};
use alloy::providers::{Identity, Provider, ProviderBuilder, RootProvider, SendableTx};
use alloy::signers::local::PrivateKeySigner;
use alloy::transports::{RpcError, TransportErrorKind};
use std::str::FromStr;
use std::time::Duration;
use tracing::{error, info, instrument, warn};

/// What `ProviderBuilder::new().with_recommended_fillers().wallet(..)` builds,
/// spelled out so it can be held in a field.
///
/// Naming it is the whole trick: the fillers carry per-provider state, and
/// [`ChainIdFiller`] in particular is a `OnceLock` that caches the chain id
/// after its first fetch. A provider rebuilt per call never gets to use it.
///
/// The nonce filler is deliberately left at its default, [`SimpleNonceManager`],
/// which re-reads the pending transaction count on every submission. Holding the
/// provider does *not* change that — and it must not: a cached nonce that
/// desyncs produces a replacement transaction, which is a bad failure mode when
/// idempotency is keyed on the transaction hash.
///
/// [`ChainIdFiller`]: alloy::providers::fillers::ChainIdFiller
/// [`SimpleNonceManager`]: alloy::providers::fillers::SimpleNonceManager
type PoolProvider = FillProvider<
    JoinFill<
        JoinFill<Identity, <Ethereum as RecommendedFillers>::RecomendedFillers>,
        WalletFiller<EthereumWallet>,
    >,
    RootProvider<HttpTransport>,
    HttpTransport,
    Ethereum,
>;

/// Alloy's request and receipt types, aliased so the paths do not crowd every
/// signature in this file.
type TxRequest = alloy::rpc::types::TransactionRequest;
type ChainReceipt = alloy::rpc::types::TransactionReceipt;

pub struct Submitter {
    pub chain_id: i64,
    pub pool_address: Address,
    pub signer_address: Address,
    pub receipt_timeout_s: u64,
    pub receipt_poll_interval_ms: u64,
    provider: PoolProvider,
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
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(EthereumWallet::from(signer))
            .on_client(rpc.client());
        provider
            .client()
            .set_poll_interval(Duration::from_millis(receipt_poll_interval_ms));
        Ok(Self {
            chain_id,
            pool_address,
            signer_address,
            receipt_timeout_s,
            receipt_poll_interval_ms,
            provider,
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
        let tx = TxRequest::default()
            .to(self.pool_address)
            .input(data.into());
        let envelope = self.fill_and_sign(tx).await?;
        // Known before the broadcast, which is what makes an unanswered send
        // resolvable against the chain instead of guessed at.
        let tx_hash = *envelope.tx_hash();
        let receipt = self.broadcast(envelope, tx_hash).await?;
        self.receipt_outcome(receipt, tx_hash)
    }

    /// Price and sign, without broadcasting.
    ///
    /// Split from [`Self::broadcast`] because the two fail differently in the
    /// one way that matters to the caller: everything here happens before the
    /// transaction is signed, so a failure proves nothing reached the mempool
    /// and the speculative leaves may be rolled back.
    async fn fill_and_sign(&self, tx: TxRequest) -> AppResult<TxEnvelope> {
        // The happy path does not probe. A contract guard that rejects the
        // payload surfaces from the gas estimate as an opaque failure, so the
        // `eth_call` that makes the revert legible is run *after* that failure
        // instead of ahead of every submission — same diagnosis, one fewer RPC
        // round trip per successful spend.
        let filled = match self.provider.fill(tx.clone()).await {
            Ok(filled) => filled,
            Err(e) => {
                let probe = self.probe_revert(&tx).await;
                return Err(classify_fill_failure(probe.as_deref(), &e.to_string()));
            }
        };
        match filled {
            SendableTx::Envelope(envelope) => Ok(envelope),
            // The wallet filler signs during `fill`, so a builder here means
            // the signer dropped out of the stack: a wiring bug, not a runtime
            // condition.
            SendableTx::Builder(_) => Err(AppError::Internal(
                "transaction was not signed during fill; wallet filler missing".into(),
            )),
        }
    }

    /// Broadcast a signed transaction and wait for one confirmation.
    ///
    /// Every path out of here either has a receipt or has established that the
    /// outcome is genuinely unknowable — never a guess.
    async fn broadcast(&self, envelope: TxEnvelope, tx_hash: B256) -> AppResult<ChainReceipt> {
        let pending = match self.provider.send_tx_envelope(envelope).await {
            Ok(pending) => pending,
            Err(e) if broadcast_refused(&e) => {
                return Err(AppError::Rpc(format!("send_transaction: {e}")));
            }
            Err(e) => {
                return self
                    .resolve_by_hash(tx_hash, Unresolved::Unanswered(e.to_string()))
                    .await;
            }
        };
        let pending = pending
            .with_required_confirmations(1)
            .with_timeout(Some(Duration::from_secs(self.receipt_timeout_s)));
        info!(%tx_hash, "tx submitted, awaiting receipt");
        match pending.get_receipt().await {
            Ok(receipt) => Ok(receipt),
            Err(e) => {
                self.resolve_by_hash(tx_hash, Unresolved::NoReceipt(e.to_string()))
                    .await
            }
        }
    }

    /// Turn a receipt into this layer's verdict.
    ///
    /// Shared by the ordinary path and by the one that recovers a receipt
    /// after an unanswered broadcast, so both classify a revert and a
    /// block-number-less receipt the same way.
    fn receipt_outcome(
        &self,
        receipt: alloy::rpc::types::TransactionReceipt,
        tx_hash: B256,
    ) -> AppResult<SubmissionReceipt> {
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

    /// One `eth_call` against the same payload, to recover the revert reason a
    /// failed send hid behind a gas-estimation error.
    async fn probe_revert(&self, tx: &alloy::rpc::types::TransactionRequest) -> Option<String> {
        match self
            .provider
            .call(&tx.clone().from(self.signer_address))
            .await
        {
            Ok(_) => None,
            Err(e) => {
                warn!(error = %e, "eth_call probe reverted");
                Some(e.to_string())
            }
        }
    }

    /// Ask the chain what happened to a transaction this layer could not
    /// observe the outcome of.
    ///
    /// Both callers arrive holding a signed transaction whose hash is known
    /// but whose fate is not — [`Unresolved`] says which. Keeping them
    /// distinct matters: "the node may never have seen this" and "the node
    /// took it and is slow" call for different responses, and it is the first
    /// thing anyone reading the log needs.
    ///
    /// [`AppError::SubmitUnknown`] parks the chain's tree mirror until a
    /// restart, so it is worth a full polling window before saying so.
    async fn resolve_by_hash(
        &self,
        tx_hash: B256,
        unresolved: Unresolved,
    ) -> AppResult<ChainReceipt> {
        warn!(
            %tx_hash,
            cause = unresolved.cause(),
            error = unresolved.detail(),
            "submission outcome unobserved; polling the chain before declaring it unknown"
        );
        let interval = Duration::from_millis(self.receipt_poll_interval_ms.max(1));
        let poll = async {
            loop {
                match self.provider.get_transaction_receipt(tx_hash).await {
                    Ok(Some(r)) => return r,
                    Ok(None) => {}
                    // A failing poll says nothing about the transaction, so
                    // keep trying until the window closes.
                    Err(e) => warn!(%tx_hash, error = %e, "receipt poll failed"),
                }
                tokio::time::sleep(interval).await;
            }
        };
        match tokio::time::timeout(Duration::from_secs(self.receipt_timeout_s), poll).await {
            Ok(receipt) => {
                info!(%tx_hash, cause = unresolved.cause(), "receipt recovered");
                Ok(receipt)
            }
            Err(_) => Err(AppError::SubmitUnknown(format!(
                "tx {tx_hash}: {}, and no receipt after polling {}s: {}",
                unresolved.cause(),
                self.receipt_timeout_s,
                unresolved.detail(),
            ))),
        }
    }
}

/// Why a submission's outcome could not be observed directly.
///
/// Carried into [`Submitter::resolve_by_hash`] so its log line and its error
/// name the real situation. Collapsing the two into one "no receipt" message
/// throws away the only clue worth having.
enum Unresolved {
    /// The node never answered the broadcast. It may hold the transaction, or
    /// may never have seen it.
    Unanswered(String),
    /// The broadcast was accepted, but no receipt arrived inside the window.
    NoReceipt(String),
}

impl Unresolved {
    /// Fixed text: safe to log, stable enough to alert on.
    fn cause(&self) -> &'static str {
        match self {
            Self::Unanswered(_) => "broadcast went unanswered",
            Self::NoReceipt(_) => "no receipt inside the first window",
        }
    }

    /// The underlying error. Node text, so logs only — it can carry the RPC
    /// URL, and with it an API key.
    fn detail(&self) -> &str {
        match self {
            Self::Unanswered(e) | Self::NoReceipt(e) => e,
        }
    }
}

/// Whether a failed broadcast proves the transaction never reached the mempool.
///
/// True only when the node answered. A JSON-RPC error object is the node
/// saying "no" — nonce too low, underpriced, insufficient funds — and nothing
/// was accepted, so the caller may roll its speculative leaves back.
///
/// Everything else is silence: a reset connection, a timeout, a gateway 5xx.
/// The node may have accepted and broadcast the transaction before the answer
/// was lost. Rolling back there truncates the mirror while the transaction
/// mines, and a mirror one advance behind the chain fails
/// `_validateBatchHeader`'s `startIndex == committedCount` check — so *every*
/// later submission reverts `BatchMisaligned`, each rolling back further. The
/// chain is then dead with symptoms that do not point at the cause, which is
/// why an unanswered broadcast is resolved against the chain instead.
fn broadcast_refused(e: &RpcError<TransportErrorKind>) -> bool {
    matches!(e, RpcError::ErrorResp(_))
}

/// Decide what a failure *while filling* a transaction actually was.
///
/// Everything here happened before the transaction was signed, so nothing
/// reached the mempool and the caller's mirror rollback is sound whatever this
/// returns. That is why the broadcast is classified separately, by
/// [`broadcast_refused`], where an unanswered send has to be resolved rather
/// than assumed.
///
/// A contract guard that rejects the payload surfaces here as a gas-estimation
/// failure — RPC-shaped, but not an RPC fault. Prefer the probe's message,
/// which states the revert plainly; otherwise sniff the fill error for the
/// same marker, since the probe can succeed against a state the transaction is
/// later priced against.
fn classify_fill_failure(probe_revert: Option<&str>, fill_err: &str) -> AppError {
    let rejected = |detail: &str| {
        revert_reason(detail).map(|reason| AppError::ContractRejected {
            reason,
            detail: detail.to_string(),
        })
    };
    probe_revert
        .and_then(rejected)
        .or_else(|| rejected(fill_err))
        .unwrap_or_else(|| AppError::Rpc(format!("fill transaction: {fill_err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::rpc::json_rpc::ErrorPayload;
    use alloy::transports::TransportErrorKind;

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
        let err = classify_fill_failure(Some(&probe_error()), "gas estimation failed");
        assert!(matches!(err, AppError::ContractRejected { .. }));
        assert!(err.status().is_client_error(), "got {}", err.status());
        assert!(err.client_message().contains("too little received"));
    }

    /// The whole point of the variant: the reason reaches the caller while the
    /// node URL that carried it does not.
    #[test]
    fn the_rejection_reason_reaches_the_caller_but_the_node_url_does_not() {
        let err = classify_fill_failure(Some(&probe_error()), "gas estimation failed");
        let msg = err.client_message();
        assert!(!msg.contains("example.com"), "leaked host: {msg}");
        assert!(!msg.contains("deadbeefsecretkey"), "leaked key: {msg}");
    }

    #[test]
    fn a_revert_the_probe_missed_is_still_a_contract_rejection() {
        let err = classify_fill_failure(None, &probe_error());
        assert!(matches!(err, AppError::ContractRejected { .. }));
    }

    /// A probe that failed for transport reasons says nothing about the
    /// payload, so it must not be reported as the caller's fault.
    ///
    /// Rollback-safe regardless: this is the fill phase, before anything is
    /// signed. The broadcast phase is the one that cannot assume — see
    /// [`broadcast_refused`].
    #[test]
    fn a_transport_failure_while_filling_stays_an_rpc_error() {
        let transport = format!("error sending request for url {NODE_URL}");
        let err = classify_fill_failure(Some(&transport), &transport);
        assert!(matches!(err, AppError::Rpc(_)));
        assert_eq!(err.client_message(), "internal error");
    }

    /// The node said no. Nothing was accepted, so the mirror may roll back.
    #[test]
    fn a_node_that_answers_no_proves_nothing_was_broadcast() {
        let refused = RpcError::ErrorResp(ErrorPayload {
            code: -32000,
            message: "nonce too low".into(),
            data: None,
        });
        assert!(broadcast_refused(&refused));
    }

    /// Silence proves nothing. Treating it as "did not land" is what
    /// truncates a mirror for a transaction that mines.
    #[test]
    fn an_unanswered_broadcast_is_never_assumed_to_have_failed() {
        let unanswered = [
            RpcError::Transport(TransportErrorKind::BackendGone),
            RpcError::Transport(TransportErrorKind::HttpError(
                alloy::transports::HttpError {
                    status: 502,
                    body: "bad gateway".into(),
                },
            )),
            RpcError::NullResp,
        ];
        for e in &unanswered {
            assert!(!broadcast_refused(e), "assumed failure for {e}");
        }
    }
}
