//! `/v1/generic`: shielded execution of arbitrary calls, submitted through
//! `GenericCallWrapper`.

mod allowlist;
mod validate;

pub use allowlist::parse_allowed_calls;
pub use validate::GenericPolicy;

use crate::adapters::abi::{IBundler, IGenericCallWrapper, IMasp};
use crate::domain::batch::PaddedBatch;
use crate::domain::dto::SubmitGenericPayload;
use crate::domain::error::AppResult;
use crate::domain::responses::EstimateResponse;
use crate::services::admission::nullifier_guard::PendingGuard;
use crate::services::fees::gas_witness::{EntryPoint, GasWitness};
use crate::services::fees::quote::FeeQuoter;
use crate::services::fees::shielded::ShieldedFeeChecker;
use crate::services::pipeline::batcher::{Batcher, BundleItem, QueuedItem};
use crate::services::pipeline::transact::{
    FeeContext, merkle_root_of, parse_spend_batch, spend_tree_for, verify_transact_proof,
};
use crate::services::submitter::SubmissionReceipt;
use crate::services::transact_verifier::TransactVerifier;
use crate::services::tree::{AdvancedState, ReservedSlot};
use ::asset_registry::AssetRegistry;
use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use crypto::tree::Field;
use std::sync::Arc;
use tracing::{info, instrument};
use validate::{check_min_gas, validate_generic};

/// Per-chain generic-call pipeline, mirroring `SwapPipeline` except that the
/// call targets `GenericCallWrapper.execute`, the payload carries the calls and
/// up to four output escrows, and the fee covers the gas the wallet reserves for
/// the call leg.
///
/// Shares the chain's batcher with the other pipelines, so a generic call can
/// land in the same transaction as their operations.
pub struct GenericPipeline {
    pub policy: GenericPolicy,
    pub batcher: Batcher,
    pub fee_quoter: Arc<FeeQuoter>,
    pub gas_witness: Arc<GasWitness>,
    /// See `SpendPipeline::transact_verifier`. Leg 1 is the same transact proof
    /// and gets the same pre-check.
    pub transact_verifier: Option<Arc<TransactVerifier>>,
    /// See `SpendPipeline::shielded_fee`. Leg 1 carries the same output slots, so
    /// a generic call pays its fee as a spend does.
    pub shielded_fee: Option<Arc<ShieldedFeeChecker>>,
    /// See `SpendPipeline::assets`.
    pub assets: Arc<AssetRegistry>,
}

/// Gas a submission forwarding `min_gas` to its call leg is quoted and charged:
/// the wrapper's own overhead plus the whole floor, since the wrapper reverts
/// unless that much is forwarded.
fn generic_gas(witness: &GasWitness, min_gas: u64) -> u64 {
    witness.gas_for(EntryPoint::Generic).saturating_add(min_gas)
}

impl GenericPipeline {
    /// The calls and tokens are left out of the span, as `SwapPipeline::process`
    /// leaves out the pair.
    #[instrument(skip_all, fields(chain_id = self.policy.binding.chain_id))]
    pub async fn process(
        &self,
        payload: SubmitGenericPayload,
        guard: PendingGuard,
    ) -> AppResult<SubmissionReceipt> {
        let (args, min_gas) = validate_generic(&payload, &self.policy)?;
        verify_transact_proof(
            self.transact_verifier.as_deref(),
            &payload.proof,
            &payload.pub_inputs,
            &payload.aux,
        )?;
        let batch = parse_spend_batch(&payload.pub_inputs)?;
        let merkle_root = merkle_root_of(&payload.pub_inputs)?;
        self.fees()
            .charge(
                &payload.pub_inputs,
                &payload.aux,
                generic_gas(&self.gas_witness, min_gas),
            )
            .await?;

        let item = GenericItem {
            nullifiers: payload.pub_inputs.nullifier.to_vec(),
            batch,
            merkle_root,
            wrapper: self.policy.binding.wrapper,
            args,
            min_gas,
        };
        let bundled = self.batcher.submit(Box::new(item), Some(guard)).await?;

        // Less the call leg's floor, so the witness tracks the wrapper's own
        // overhead and the next quote adds that submission's floor to it.
        self.gas_witness.observe(
            EntryPoint::Generic,
            bundled.receipt.gas_used.saturating_sub(min_gas),
        );
        info!(
            tx_hash = %bundled.receipt.tx_hash,
            gas_used = bundled.receipt.gas_used,
            min_gas,
            index = bundled.index,
            bundle_size = bundled.bundle_size,
            "generic call submitted"
        );
        Ok(bundled.receipt)
    }

    /// Fee quote for `/v1/generic/estimate`: what a submission carrying
    /// `min_gas` as its `generic.minGas` is charged. See `SpendPipeline::estimate`.
    pub async fn estimate(&self, min_gas: u64) -> AppResult<EstimateResponse> {
        let min_gas = check_min_gas(U256::from(min_gas), self.policy.max_min_gas)?;
        self.fees()
            .quote(generic_gas(&self.gas_witness, min_gas))
            .await
    }

    fn fees(&self) -> FeeContext<'_> {
        FeeContext {
            chain_id: self.policy.binding.chain_id,
            fee_quoter: &self.fee_quoter,
            assets: &self.assets,
            shielded_fee: self.shielded_fee.as_deref(),
        }
    }
}

/// A generic call, as the batcher bundles it.
struct GenericItem {
    nullifiers: Vec<String>,
    batch: PaddedBatch,
    merkle_root: Field,
    wrapper: Address,
    /// Everything but `tp_w` and `tpi_w`, which depend on the reserved slot.
    args: IGenericCallWrapper::GenericArgs,
    /// `args.minGas`, as gas units.
    min_gas: u64,
}

impl BundleItem for GenericItem {
    fn entry(&self) -> EntryPoint {
        EntryPoint::Generic
    }

    fn batch(&self) -> &PaddedBatch {
        &self.batch
    }

    fn merkle_root(&self) -> Option<Field> {
        Some(self.merkle_root)
    }

    fn encode(
        &self,
        slot: &ReservedSlot,
        advanced: &AdvancedState,
        digest: U256,
        tp: IMasp::Proof,
    ) -> AppResult<IBundler::Call> {
        let data = IGenericCallWrapper::executeCall {
            a: IGenericCallWrapper::GenericArgs {
                tp_w: tp,
                tpi_w: spend_tree_for(slot, advanced, digest)?,
                ..self.args.clone()
            },
        }
        .abi_encode();
        Ok(IBundler::Call {
            target: self.wrapper,
            data: data.into(),
        })
    }

    fn gas_weight(&self, witness: &GasWitness) -> u64 {
        generic_gas(witness, self.min_gas)
    }

    fn view(&self) -> QueuedItem {
        QueuedItem {
            kind: EntryPoint::Generic.as_str(),
            nullifiers: self.nullifiers.clone(),
            deposit_ids: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
