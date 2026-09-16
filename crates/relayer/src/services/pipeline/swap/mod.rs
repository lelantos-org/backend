//! `/v1/swap`: leg 1 of a shielded swap, submitted through `SwapWrapper`.

mod validate;

pub use validate::refund_address_error;

use crate::adapters::abi::{IBundler, IMasp, ISwapWrapper};
use crate::adapters::parse::parse_address;
use crate::domain::dto::SubmitSwapPayload;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::EstimateResponse;
use crate::services::admission::nullifier_guard::PendingGuard;
use crate::services::fees::gas_witness::{EntryPoint, GasWitness};
use crate::services::fees::quote::FeeQuoter;
use crate::services::fees::shielded::ShieldedFeeChecker;
use crate::services::pipeline::batcher::{Batcher, BundleItem, QueuedItem};
use crate::services::pipeline::transact::{
    FeeContext, SpendInputs, TransactBinding, merkle_root_of, parse_spend_inputs, spend_tree_for,
    spend_witness, verify_transact_proof,
};
use crate::services::submitter::SubmissionReceipt;
use crate::services::transact_verifier::TransactVerifier;
use crate::services::tree::{AdvancedState, ReservedSlot};
use ::asset_registry::AssetRegistry;
use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use crypto::tree::Field;
use groth16::TreeUpdateBatchWitness;
use std::sync::Arc;
use tracing::{info, instrument};
use validate::{validate_refund_to, validate_swap_shape};

/// Per-chain swap pipeline, mirroring `SpendPipeline` except that:
///
/// - the leg-1 SNARK public inputs travel inside `SwapWrapper.SwapArgs.pi_w`
///   rather than the bare MASP entry point;
/// - the call targets the wrapper rather than the pool;
/// - leg-2 escrow data (`deposit_d`, `aux_d`) rides in the same calldata blob,
///   with no separate Permit2 pull and no SNARK at submit.
///
/// Shares the chain's batcher with `SpendPipeline` and `FlushPipeline`, so a swap
/// can land in the same transaction as their operations.
pub struct SwapPipeline {
    pub chain_id: i64,
    pub batcher: Batcher,
    /// Cached for shape validation: `pi_w.recipient` and `deposit_d.payer` must
    /// both equal this. The wrapper enforces it on chain too, but rejecting here
    /// gives a 400 instead of a wasted Groth16 and a revert.
    pub wrapper_address: Address,
    /// This relayer's `Bundler`, the wrapper's caller, and so the address `pi_w.payer`
    /// must name: the wrapper only lets that address drive the swap.
    pub bundler_address: Address,
    pub fee_quoter: Arc<FeeQuoter>,
    pub gas_witness: Arc<GasWitness>,
    /// See `SpendPipeline::transact_verifier`. Leg 1 of a swap is the same transact
    /// proof and gets the same pre-check.
    pub transact_verifier: Option<Arc<TransactVerifier>>,
    /// See `SpendPipeline::shielded_fee`. Leg 1 carries the same output slots, so
    /// a swap pays its fee as a spend does.
    pub shielded_fee: Option<Arc<ShieldedFeeChecker>>,
    /// See `SpendPipeline::assets`.
    pub assets: Arc<AssetRegistry>,
}

impl SwapPipeline {
    /// Neither `start_index` nor the token pair is recorded. See
    /// `SpendPipeline::process` for `start_index`; the pair is omitted for the same
    /// reason `metaquoter`'s `post_quote` omits it, since a quote and a swap from
    /// one client are already adjacent in the access log and naming the pair in
    /// both turns that adjacency into a trade record. `adapter` remains: it
    /// identifies the venue rather than the trade.
    #[instrument(
        skip_all,
        fields(chain_id = self.chain_id, adapter = %payload.swap.adapter),
    )]
    pub async fn process(
        &self,
        payload: SubmitSwapPayload,
        guard: PendingGuard,
    ) -> AppResult<SubmissionReceipt> {
        let args = self.validate(&payload)?;
        verify_transact_proof(
            self.transact_verifier.as_deref(),
            &payload.proof,
            &payload.pub_inputs,
            &payload.aux,
        )?;
        let inputs = parse_spend_inputs(&payload.pub_inputs)?;
        let merkle_root = merkle_root_of(&payload.pub_inputs)?;
        self.fees()
            .charge(
                &payload.pub_inputs,
                &payload.aux,
                self.gas_witness.gas_for(EntryPoint::Swap),
            )
            .await?;

        let item = SwapItem {
            payload,
            inputs,
            merkle_root,
            wrapper: self.wrapper_address,
            args,
        };
        let bundled = self.batcher.submit(Box::new(item), Some(guard)).await?;

        self.gas_witness
            .observe(EntryPoint::Swap, bundled.receipt.gas_used);
        info!(
            tx_hash = %bundled.receipt.tx_hash,
            gas_used = bundled.receipt.gas_used,
            index = bundled.index,
            bundle_size = bundled.bundle_size,
            "swap submitted"
        );
        Ok(bundled.receipt)
    }

    /// Fee quote for `/v1/swap/estimate`. See `SpendPipeline::estimate`: no mirror
    /// access, no prove and no payload, since every swap prices as
    /// `EntryPoint::Swap`.
    pub async fn estimate(&self) -> AppResult<EstimateResponse> {
        self.fees()
            .quote(self.gas_witness.gas_for(EntryPoint::Swap))
            .await
    }

    fn fees(&self) -> FeeContext<'_> {
        FeeContext {
            chain_id: self.chain_id,
            fee_quoter: &self.fee_quoter,
            assets: &self.assets,
            shielded_fee: self.shielded_fee.as_deref(),
        }
    }

    /// Returns the parsed `SwapArgs`, minus the tree proof only the batcher can
    /// fill, so the calldata encoder does not parse the payload a second time.
    fn validate(&self, payload: &SubmitSwapPayload) -> AppResult<ISwapWrapper::SwapArgs> {
        // The wrapper calls `MASP.withdraw` for leg 1, so the wrapper rather than
        // this relayer's Bundler is the pool's `msg.sender`, and the pool checks
        // `pi.relayer == msg.sender`. Binding to the Bundler here would reject every
        // correctly built swap with a 400.
        let binding = TransactBinding {
            chain_id: self.chain_id,
            relayer: self.wrapper_address,
        };
        let args = validate_swap_shape(payload, self.wrapper_address, binding)?;
        // The Bundler calls the wrapper, which lets only `pi.payer` drive the swap.
        let payer = parse_address(&payload.pub_inputs.payer)?;
        if payer != self.bundler_address {
            return Err(AppError::BadRequest(format!(
                "pubInputs.payer ({payer}) must equal this relayer's submitter ({})",
                self.bundler_address
            )));
        }
        validate_refund_to(args.refundTo, self.wrapper_address, self.bundler_address)?;
        Ok(args)
    }
}

/// A shielded swap, as the batcher bundles it.
struct SwapItem {
    payload: SubmitSwapPayload,
    inputs: SpendInputs,
    merkle_root: Field,
    wrapper: Address,
    /// Everything but `tp_w` and `tpi_w`, which depend on the reserved slot.
    args: ISwapWrapper::SwapArgs,
}

impl BundleItem for SwapItem {
    fn entry(&self) -> EntryPoint {
        EntryPoint::Swap
    }

    fn leaves(&self) -> Vec<(Field, [U256; 2])> {
        self.inputs.leaves()
    }

    fn merkle_root(&self) -> Option<Field> {
        Some(self.merkle_root)
    }

    fn witness(&self, slot: &ReservedSlot, advanced: &AdvancedState) -> TreeUpdateBatchWitness {
        spend_witness(slot, advanced, &self.inputs)
    }

    fn encode(
        &self,
        slot: &ReservedSlot,
        advanced: &AdvancedState,
        tp: IMasp::Proof,
    ) -> AppResult<IBundler::Call> {
        let data = ISwapWrapper::swapCall {
            a: ISwapWrapper::SwapArgs {
                tp_w: tp,
                tpi_w: spend_tree_for(slot, advanced)?,
                ..self.args.clone()
            },
        }
        .abi_encode();
        Ok(IBundler::Call {
            target: self.wrapper,
            data: data.into(),
        })
    }

    fn view(&self) -> QueuedItem {
        QueuedItem {
            kind: EntryPoint::Swap.as_str(),
            nullifiers: self.payload.pub_inputs.nullifier.to_vec(),
            deposit_ids: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
