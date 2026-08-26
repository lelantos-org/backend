//! Submits one MASP call per chain. Nonces are sequential because the pipeline
//! mutex serialises per-chain submissions.
//!
//! The provider is built once at construction. Alloy's filler stack is deeply
//! generic and resists `dyn Provider` boxing, but the type can be named, and
//! rebuilding it per submission would discard `ChainIdFiller`'s cache and pay an
//! `eth_chainId` round trip before every transaction. See [`PoolProvider`].

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
/// Naming the type is what allows it to be held: the fillers carry per-provider
/// state, and [`ChainIdFiller`] is a `OnceLock` caching the chain id after its
/// first fetch, which a provider rebuilt per call never reuses.
///
/// The nonce filler stays at its default, [`SimpleNonceManager`], which re-reads
/// the pending transaction count on every submission. Holding the provider does
/// not change that, and must not: a cached nonce that desyncs produces a
/// replacement transaction, which is a poor failure mode when idempotency is
/// keyed on the transaction hash.
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
    /// Block the transaction mined in. Some chains omit this in pre-finality
    /// receipts, but the relayer waits for one confirmation, so it should be
    /// present.
    pub block_number: i64,
    /// Gas units used by the executed transaction, a post-EIP-1559 receipt field.
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

    /// Submit ABI-encoded calldata to the MASP pool and await one confirmation.
    /// The pipeline picks the encoding — `flushBatch`, `transfer`, `withdraw` or
    /// `withdrawNative` — and passes the bytes here, keeping this layer agnostic
    /// of the call shape. None of the supported entry points are payable.
    ///
    /// The failure modes are distinct because the caller's tree-mirror rollback is
    /// sound only for some of them:
    ///   - [`AppError::Rpc`]: the node refused the broadcast; nothing landed.
    ///   - [`AppError::Reverted`]: mined but reverted; no leaves inserted.
    ///   - [`AppError::SubmitUnknown`]: broadcast accepted with no receipt inside
    ///     the timeout. It may still mine, so do not roll back.
    #[instrument(
        skip_all,
        fields(chain_id = self.chain_id, pool = %self.pool_address, calldata_len = data.len()),
    )]
    pub async fn submit(&self, data: Vec<u8>) -> AppResult<SubmissionReceipt> {
        let tx = TxRequest::default()
            .to(self.pool_address)
            .input(data.into());
        let envelope = self.fill_and_sign(tx).await?;
        // Known before the broadcast, which makes an unanswered send resolvable
        // against the chain rather than guessed at.
        let tx_hash = *envelope.tx_hash();
        let receipt = self.broadcast(envelope, tx_hash).await?;
        self.receipt_outcome(receipt, tx_hash)
    }

    /// Price and sign, without broadcasting.
    ///
    /// Split from [`Self::broadcast`] because the two fail differently in the way
    /// that matters to the caller: everything here happens before the transaction
    /// is signed, so a failure proves nothing reached the mempool and the
    /// speculative leaves may be rolled back.
    async fn fill_and_sign(&self, tx: TxRequest) -> AppResult<TxEnvelope> {
        // The success path does not probe. A contract guard rejecting the payload
        // surfaces from the gas estimate as an opaque failure, so the `eth_call`
        // that makes the revert legible runs after that failure rather than ahead
        // of every submission: the same diagnosis, one fewer RPC round trip per
        // successful spend.
        let filled = match self.provider.fill(tx.clone()).await {
            Ok(filled) => filled,
            Err(e) => {
                let probe = self.probe_revert(&tx).await;
                return Err(classify_fill_failure(probe.as_deref(), &e.to_string()));
            }
        };
        match filled {
            SendableTx::Envelope(envelope) => Ok(envelope),
            // The wallet filler signs during `fill`, so a builder here means the
            // signer dropped out of the stack: a wiring bug rather than a runtime
            // condition.
            SendableTx::Builder(_) => Err(AppError::Internal(
                "transaction was not signed during fill; wallet filler missing".into(),
            )),
        }
    }

    /// Broadcast a signed transaction and wait for one confirmation.
    ///
    /// Every path out of here either holds a receipt or has established that the
    /// outcome is unknowable.
    async fn broadcast(&self, envelope: TxEnvelope, tx_hash: B256) -> AppResult<ChainReceipt> {
        let pending = match self.provider.send_tx_envelope(envelope).await {
            Ok(pending) => pending,
            Err(e) if broadcast_refused(&e) => {
                return Err(AppError::Rpc(format!("send_transaction: {e}")));
            }
            Err(e) => {
                let unresolved = if matches!(&e, RpcError::ErrorResp(_)) {
                    Unresolved::Refused(e.to_string())
                } else {
                    Unresolved::Unanswered(e.to_string())
                };
                return self.resolve_by_hash(tx_hash, unresolved).await;
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
    /// Shared by the ordinary path and by the one recovering a receipt after an
    /// unanswered broadcast, so both classify a revert and a receipt without a
    /// block number identically.
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
        // A receipt without a block number still means the transaction executed,
        // so this is an unknown-outcome failure rather than a clean one.
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
    /// Both callers arrive holding a signed transaction whose hash is known but
    /// whose fate is not; [`Unresolved`] records which case applies. The two — the
    /// node may never have seen it, or the node took it and is slow — call for
    /// different responses and are the first thing a log reader needs.
    ///
    /// [`AppError::SubmitUnknown`] parks the chain's tree mirror until a restart,
    /// so a full polling window is spent before reporting it.
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
                    // A failing poll says nothing about the transaction, so polling
                    // continues until the window closes.
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
/// Carried into [`Submitter::resolve_by_hash`] so its log line and error name the
/// actual situation; collapsing the two into one no-receipt message discards the
/// distinction.
enum Unresolved {
    /// The node never answered the broadcast. It may hold the transaction, or
    /// may never have seen it.
    Unanswered(String),
    /// The broadcast was accepted, but no receipt arrived inside the window.
    NoReceipt(String),
    /// The node answered no, with a complaint that a transaction of ours already
    /// occupying that nonce would also produce; see [`broadcast_refused`].
    Refused(String),
}

impl Unresolved {
    /// Fixed text: safe to log, stable enough to alert on.
    fn cause(&self) -> &'static str {
        match self {
            Self::Unanswered(_) => "broadcast went unanswered",
            Self::NoReceipt(_) => "no receipt inside the first window",
            Self::Refused(_) => "node refused the broadcast; it may already have mined",
        }
    }

    /// The underlying error. Node text, for logs only: it can carry the RPC URL
    /// and its API key.
    fn detail(&self) -> &str {
        match self {
            Self::Unanswered(e) | Self::NoReceipt(e) | Self::Refused(e) => e,
        }
    }
}

/// Whether a failed broadcast proves the transaction never reached the mempool.
///
/// True when the node answered no for a reason that cannot describe a transaction
/// of ours already on the chain: underpriced, insufficient funds, an invalid
/// payload. Nothing was accepted, so the caller may roll its speculative leaves
/// back.
///
/// Not true of a nonce complaint, despite it being an ordinary JSON-RPC error.
/// The transport retries `eth_sendRawTransaction`, and on a fast chain the first
/// send can already be in a block by the time the retry goes out, so the node
/// answers "nonce too low" about our own mined transaction rather than "already
/// known". Treating that as proof would roll the mirror back under a landed
/// submission.
///
/// Everything else is silence: a reset connection, a timeout, a gateway 5xx. The
/// node may have accepted and broadcast the transaction before the answer was
/// lost. Rolling back there truncates the mirror while the transaction mines, and
/// a mirror one advance behind the chain fails `_validateBatchHeader`'s
/// `startIndex == committedCount` check, so every later submission reverts
/// `BatchMisaligned` and rolls back further. Both an unanswered broadcast and an
/// inconclusive refusal are therefore resolved against the chain.
fn broadcast_refused(e: &RpcError<TransportErrorKind>) -> bool {
    match e {
        RpcError::ErrorResp(payload) => !may_describe_a_landed_tx(&payload.message),
        _ => false,
    }
}

/// Whether a node's rejection could be about a transaction of ours that has
/// already been accepted or mined, rather than about this send failing.
///
/// Matched on message text, because no JSON-RPC code distinguishes these:
/// `-32000` covers all of them.
fn may_describe_a_landed_tx(message: &str) -> bool {
    const MARKERS: [&str; 6] = [
        "nonce too low",
        "nonce has already been used",
        "already known",
        "known transaction",
        "already imported",
        "replacement transaction underpriced",
    ];
    let message = message.to_ascii_lowercase();
    MARKERS.iter().any(|m| message.contains(m))
}

/// Decide what a failure *while filling* a transaction actually was.
///
/// Everything here happened before the transaction was signed, so nothing reached
/// the mempool and the caller's mirror rollback is sound whatever this returns.
/// The broadcast is classified separately by [`broadcast_refused`], where an
/// unanswered send must be resolved rather than assumed.
///
/// A contract guard rejecting the payload surfaces here as a gas-estimation
/// failure: RPC-shaped, but not an RPC fault. The probe's message states the
/// revert plainly and is preferred; otherwise the fill error is matched for the
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

    /// The reason reaches the caller while the node URL that carried it does not.
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

    /// A probe that failed for transport reasons says nothing about the payload,
    /// so it must not be reported as the caller's fault.
    ///
    /// Rollback-safe either way: this is the fill phase, before anything is signed.
    /// The broadcast phase cannot assume; see [`broadcast_refused`].
    #[test]
    fn a_transport_failure_while_filling_stays_an_rpc_error() {
        let transport = format!("error sending request for url {NODE_URL}");
        let err = classify_fill_failure(Some(&transport), &transport);
        assert!(matches!(err, AppError::Rpc(_)));
        assert_eq!(err.client_message(), "internal error");
    }

    fn error_resp(message: &str) -> RpcError<TransportErrorKind> {
        RpcError::ErrorResp(ErrorPayload {
            code: -32000,
            message: message.to_string().into(),
            data: None,
        })
    }

    /// The node said no for a reason that cannot describe our own transaction, so
    /// nothing was accepted and the mirror may roll back.
    #[test]
    fn a_node_that_answers_no_proves_nothing_was_broadcast() {
        assert!(broadcast_refused(&error_resp(
            "insufficient funds for gas * price + value"
        )));
        assert!(broadcast_refused(&error_resp("transaction underpriced")));
    }

    /// The retry layer resends the signed envelope, so on a fast chain the node
    /// can be complaining about the first send having already mined. Rolling back
    /// there would truncate the mirror under a landed submission.
    #[test]
    fn a_nonce_complaint_never_proves_the_transaction_did_not_land() {
        let inconclusive = [
            "nonce too low: address 0x5Fde731cD64f4D22BD0Ab6Fe690C8a19E5fA4BC8, tx: 35 state: 36",
            "nonce has already been used",
            "already known",
            "known transaction: 0xdead",
            "transaction already imported",
            "replacement transaction underpriced",
        ];
        for message in inconclusive {
            assert!(
                !broadcast_refused(&error_resp(message)),
                "assumed failure for {message}"
            );
        }
    }

    /// Silence proves nothing; treating it as a failure truncates the mirror for a
    /// transaction that mines.
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
