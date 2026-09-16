//! `/v1/spend`: transfers, withdraws and native unshields.

use crate::adapters::abi::{IBundler, IMasp, INativeAdapter};
use crate::adapters::calldata::{build_aux, build_proof, build_pub_inputs};
use crate::adapters::parse::parse_address;
use crate::domain::dto::{SpendKind, SubmitSpendPayload};
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
use crate::services::tree::{AdvancedState, MirrorSnapshot, ReservedSlot};
use ::asset_registry::AssetRegistry;
use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use crypto::tree::Field;
use groth16::TreeUpdateBatchWitness;
use std::sync::Arc;
use tracing::{info, instrument};

pub struct SpendPipeline {
    pub chain_id: i64,
    /// Lock-free view of the chain's tree mirror, so `/chains` does not queue
    /// behind a bundle holding it through prove and confirmation.
    pub snapshot: Arc<MirrorSnapshot>,
    /// The chain's batcher, which reserves, proves and submits.
    pub batcher: Batcher,
    pub pool_address: Address,
    /// This relayer's `Bundler`: the pool's caller on a spend, so the address a
    /// transfer or withdraw proof names as `relayer`.
    pub bundler_address: Address,
    /// Advertised on `/chains` as `refundAddress`; see `ChainCfg::refund_address`.
    /// Not checked against spends, which carry no `refundTo`.
    pub refund_address: Address,
    /// Built only for chains with a configured `native_adapter_address`. Without
    /// it, `withdrawNative` payloads are rejected. The adapter calls
    /// `MASP.withdraw` itself, so a native unshield names it as `relayer`.
    pub native_adapter: Option<Address>,
    pub fee_quoter: Arc<FeeQuoter>,
    pub gas_witness: Arc<GasWitness>,
    /// Checks the wallet's transact proof before the prover and the mirror lock
    /// are spent on it. `None` when the deployment shipped no transact verification
    /// key; see `ProverCfg::transact_vkey_path`.
    pub transact_verifier: Option<Arc<TransactVerifier>>,
    /// Requires each submission to carry a shielded fee note. `None` on a chain
    /// with none configured, where the relayer pays gas from its own signer.
    pub shielded_fee: Option<Arc<ShieldedFeeChecker>>,
    /// Resolves a fee token's ERC-20 address to its MASP asset id and scale,
    /// so an estimate can tell a client what note value to build.
    pub assets: Arc<AssetRegistry>,
}

impl SpendPipeline {
    /// `start_index` is absent from this span. It is the tree slot this caller's
    /// outputs land in, and pairing it with a timestamp maps a submission to its
    /// leaves. The chain publishes it through
    /// `CommitmentTree.RootAdvanced(uint64 indexed startIndex, …)`, so omitting it
    /// denies an observer nothing they cannot already read, while keeping the
    /// correlation out of the relayer's own logs.
    #[instrument(skip_all, fields(chain_id = self.chain_id, kind = ?payload.kind))]
    pub async fn process(
        &self,
        payload: SubmitSpendPayload,
        guard: PendingGuard,
    ) -> AppResult<SubmissionReceipt> {
        self.validate(&payload)?;
        verify_transact_proof(
            self.transact_verifier.as_deref(),
            &payload.proof,
            &payload.pub_inputs,
            &payload.aux,
        )?;
        let inputs = parse_spend_inputs(&payload.pub_inputs)?;
        let merkle_root = merkle_root_of(&payload.pub_inputs)?;
        let entry = EntryPoint::from(payload.kind);
        let target = self.target_for(payload.kind)?;

        self.fees()
            .charge(
                &payload.pub_inputs,
                &payload.aux,
                self.gas_witness.gas_for(entry),
            )
            .await?;

        let item = SpendItem {
            payload,
            inputs,
            merkle_root,
            target,
        };
        let bundled = self.batcher.submit(Box::new(item), Some(guard)).await?;

        self.gas_witness.observe(entry, bundled.receipt.gas_used);
        info!(
            entry = entry.as_str(),
            tx_hash = %bundled.receipt.tx_hash,
            gas_used = bundled.receipt.gas_used,
            index = bundled.index,
            bundle_size = bundled.bundle_size,
            "spend submitted"
        );
        Ok(bundled.receipt)
    }

    /// Fee quote for `/v1/spend/estimate`, pricing this entry point's observed
    /// gas. Touches neither the tree mirror nor the prover, so it cannot stall a
    /// real submission.
    ///
    /// Takes a `SpendKind` rather than a payload: the answer depends on the entry
    /// point alone, so a full `SubmitSpendPayload` would only add a shape check on
    /// a spend the caller may never submit. See `EstimateSpendRequest`. There is
    /// therefore no pre-flight, and an estimate does not tell a wallet whether its
    /// spend would revert on chain.
    pub async fn estimate(&self, kind: SpendKind) -> AppResult<EstimateResponse> {
        self.fees()
            .quote(self.gas_witness.gas_for(EntryPoint::from(kind)))
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

    fn validate(&self, payload: &SubmitSpendPayload) -> AppResult<()> {
        validate_spend_shape(payload, self.binding(payload.kind)?, self.native_adapter)
    }

    /// The address the proof must name as `relayer`: this relayer's `Bundler` for
    /// pool-targeted calls, and the adapter for a native unshield.
    fn binding(&self, kind: SpendKind) -> AppResult<TransactBinding> {
        let relayer = match kind {
            SpendKind::WithdrawNative => self.native_adapter()?,
            _ => self.bundler_address,
        };
        Ok(TransactBinding {
            chain_id: self.chain_id,
            relayer,
        })
    }

    fn native_adapter(&self) -> AppResult<Address> {
        self.native_adapter.ok_or_else(|| {
            AppError::BadRequest(format!(
                "withdrawNative is not available on chain {}: no native_adapter_address configured",
                self.chain_id
            ))
        })
    }

    /// The contract the Bundler calls for this kind.
    fn target_for(&self, kind: SpendKind) -> AppResult<Address> {
        Ok(match kind {
            SpendKind::WithdrawNative => self.native_adapter()?,
            _ => self.pool_address,
        })
    }
}

/// A transfer, withdraw or native unshield, as the batcher bundles it.
struct SpendItem {
    payload: SubmitSpendPayload,
    inputs: SpendInputs,
    merkle_root: Field,
    target: Address,
}

impl BundleItem for SpendItem {
    fn entry(&self) -> EntryPoint {
        EntryPoint::from(self.payload.kind)
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
        Ok(IBundler::Call {
            target: self.target,
            data: encode_spend_calldata(&self.payload, slot, advanced, tp)?.into(),
        })
    }

    fn view(&self) -> QueuedItem {
        QueuedItem {
            kind: self.entry().as_str(),
            nullifiers: self.payload.pub_inputs.nullifier.to_vec(),
            deposit_ids: Vec::new(),
        }
    }
}

fn encode_spend_calldata(
    payload: &SubmitSpendPayload,
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    tp: IMasp::Proof,
) -> AppResult<Vec<u8>> {
    let p = build_proof(&payload.proof)?;
    let pi = build_pub_inputs(&payload.pub_inputs)?;
    let tpi = spend_tree_for(slot, advanced)?;
    let aux = build_aux(&payload.aux)?;
    let out = match payload.kind {
        SpendKind::Transfer => IMasp::transferCall {
            p,
            pi,
            tp,
            tpi,
            aux,
        }
        .abi_encode(),
        SpendKind::Withdraw => IMasp::withdrawCall {
            p,
            pi,
            tp,
            tpi,
            aux,
        }
        .abi_encode(),
        // The same argument tuple with a different callee: the adapter forwards it
        // to `MASP.withdraw` and unwraps the proceeds.
        SpendKind::WithdrawNative => INativeAdapter::withdrawNativeCall {
            p,
            pi,
            tp,
            tpi,
            aux,
        }
        .abi_encode(),
    };
    Ok(out)
}

fn validate_spend_shape(
    payload: &SubmitSpendPayload,
    binding: TransactBinding,
    native_adapter: Option<Address>,
) -> AppResult<()> {
    binding.check(&payload.pub_inputs)?;
    if payload.pub_inputs.public_in != 0 {
        return Err(AppError::BadRequest(
            "spend payload must have publicIn == 0".into(),
        ));
    }
    match payload.kind {
        SpendKind::Transfer => {
            if payload.pub_inputs.public_out != 0 {
                return Err(AppError::BadRequest(
                    "transfer requires publicOut == 0".into(),
                ));
            }
        }
        SpendKind::Withdraw => {
            if payload.pub_inputs.public_out == 0 {
                return Err(AppError::BadRequest(
                    "withdraw requires publicOut > 0".into(),
                ));
            }
        }
        SpendKind::WithdrawNative => {
            if payload.pub_inputs.public_out == 0 {
                return Err(AppError::BadRequest(
                    "withdrawNative requires publicOut > 0".into(),
                ));
            }
            // Otherwise the adapter reverts `AdapterNotRecipient`: the ERC-20
            // proceeds must land on it so it can unwrap them.
            let adapter = native_adapter.ok_or_else(|| {
                AppError::BadRequest("withdrawNative is not available on this chain".into())
            })?;
            let recipient = parse_address(&payload.pub_inputs.recipient)?;
            if recipient != adapter {
                return Err(AppError::BadRequest(format!(
                    "withdrawNative requires pi.recipient ({recipient}) to equal the native adapter ({adapter})"
                )));
            }
        }
    }
    // Parse every caller-supplied field the calldata encoder parses, here,
    // before anything expensive or stateful runs. The batcher encodes the call
    // after reserving the whole bundle's leaves, so a malformed `proof` or `aux`
    // found there would cost every operation in it a retry. The same errors
    // surface, just ahead of the work instead of behind it.
    //
    // This is also the only thing that rejects a malformed proof on a chain
    // with no `prover.transact_vkey_path`: `verify_transact_proof` returns
    // `Ok(())` immediately when no verifier is loaded.
    build_proof(&payload.proof)?;
    build_pub_inputs(&payload.pub_inputs)?;
    build_aux(&payload.aux)?;
    Ok(())
}

#[cfg(test)]
mod tests;
