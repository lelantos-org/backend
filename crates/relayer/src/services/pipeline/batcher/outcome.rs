//! What a bundle's `execute` said: its return data, its events and the revert
//! data of the item it stopped at.

use crate::adapters::abi::{IBundler, IMasp};
use crate::domain::error::AppError;
use crate::services::submitter::SubmissionReceipt;
use alloy::primitives::{Address, Bytes, U256, keccak256};
use alloy::sol_types::SolCall;

/// Where a bundle stopped, and why.
pub(super) struct ItemFailure {
    pub(super) index: usize,
    pub(super) reason: Bytes,
}

/// The placeholder a dry run encodes in place of a real tree-update proof.
pub(crate) fn zero_proof() -> IMasp::Proof {
    IMasp::Proof {
        a: [U256::ZERO; 2],
        b: [[U256::ZERO; 2]; 2],
        c: [U256::ZERO; 2],
    }
}

pub(super) fn execute_calldata(calls: Vec<IBundler::Call>) -> Vec<u8> {
    IBundler::executeCall { calls }.abi_encode()
}

/// The first failing item from `execute`'s return data, or `None` if all
/// `total` executed or the data does not decode.
pub(super) fn decode_execute(out: &[u8], total: usize) -> Option<ItemFailure> {
    let ret = IBundler::executeCall::abi_decode_returns(out, true).ok()?;
    let executed = ret.executed.saturating_to::<usize>();
    (executed < total).then(|| ItemFailure {
        index: executed,
        reason: ret.reason,
    })
}

/// How far a mined bundle got, from the Bundler's own events: the number of items
/// executed, `None` if the receipt carries no `BundleExecuted`, and, if it
/// stopped, the failing item's revert data.
pub(super) fn decode_logs(
    receipt: &SubmissionReceipt,
    bundler: Address,
    total: usize,
) -> (Option<usize>, Option<Bytes>) {
    let mut executed = None;
    let mut reason = None;
    for log in receipt.logs.iter().filter(|l| l.address() == bundler) {
        if let Ok(e) = log.log_decode::<IBundler::BundleExecuted>() {
            executed = Some(e.inner.data.executed.saturating_to::<usize>().min(total));
        } else if let Ok(e) = log.log_decode::<IBundler::BundleItemFailed>() {
            reason = Some(e.inner.data.reason.clone());
        }
    }
    (executed, reason)
}

/// What a failing item's revert data means for the batcher and its caller.
pub(super) struct Failure {
    /// The chain's root or leaf count moved, so no item of this bundle could land.
    pub(super) stale_root: bool,
    pub(super) error: AppError,
}

/// Decode a failing call's revert data into the error its caller sees.
///
/// Only the selector is matched: the caller learns which check failed, which is
/// what it can act on, and the arguments stay in the logs.
pub(super) fn classify(reason: &[u8]) -> Failure {
    let detail = format!("0x{}", hex::encode(reason));
    let name = error_name(reason);
    let stale_root = matches!(name.as_deref(), Some("StaleOldRoot" | "BatchMisaligned"));
    let error = match name {
        Some(reason) => AppError::ContractRejected { reason, detail },
        None => AppError::Reverted(format!("bundled operation reverted: {detail}")),
    };
    Failure { stale_root, error }
}

/// The name of the custom error, or the message of an `Error(string)`, that
/// `reason` encodes.
pub(super) fn error_name(reason: &[u8]) -> Option<String> {
    const ERRORS: &[&str] = &[
        "StaleOldRoot()",
        "BatchMisaligned()",
        "TreeFull()",
        "DoubleSpend()",
        "DuplicateNullifier()",
        "BadRelayer()",
        "UnknownRoot()",
        "ProofRejected()",
        "TreeUpdateRejected()",
        "BadChainId()",
        "UnknownAsset(uint64)",
        "AssetDisabled(uint64)",
        "SpendsPaused(uint256)",
        "DepositNotPending(uint256)",
        "DigestMismatch(uint256)",
        "BadDepositMode()",
        "ZeroRecipient()",
        "ZeroPayer()",
        "PublicOutTooLarge()",
        "MustHaveWithdraw()",
        "MustNotHaveWithdraw()",
        "MustNotHaveDeposit()",
        "AdapterNotRecipient()",
        "AdapterNotRelayer()",
        "NothingUnshielded()",
        "NativeTransferFailed()",
        "UnauthorizedSwapCaller(address,address)",
        "WrapperNotRecipient()",
        "WrapperNotRelayer()",
        "WrapperNotPayer()",
        "AdapterNotAllowed()",
        "SwapExpired()",
        "InsufficientOut(uint256,uint256)",
        "InsufficientWithdraw(uint256,uint256)",
        "LeftoverBalance(address,uint256)",
        "VenueDrained(uint64,uint256,uint256)",
        "MalformedCall(uint256)",
        "ItemOutOfGas()",
        "VenueOutOfGas()",
    ];
    let selector = reason.get(..4)?;
    if selector == [0x08, 0xc3, 0x79, 0xa0] {
        return <String as alloy::sol_types::SolValue>::abi_decode(&reason[4..], true).ok();
    }
    ERRORS.iter().find_map(|sig| {
        let hash = keccak256(sig.as_bytes());
        (hash[..4] == *selector).then(|| sig[..sig.find('(').unwrap_or(sig.len())].to_string())
    })
}
