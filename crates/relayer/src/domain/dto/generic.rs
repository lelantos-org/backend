use crate::domain::dto::deposit::DepositRequestDto;
use crate::domain::dto::transact::{OutputAuxDto, ProofDto, PubInputsDto, TRANSACT_OUT};
use serde::Deserialize;

/// Wallet-to-relayer wire format for `/v1/generic`.
///
/// The proof, public-input and aux fields are those of a `withdraw` spend whose
/// `recipient` and `relayer` are the GenericCallWrapper.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubmitGenericPayload {
    pub chain_id: i64,
    /// Leg-1 transact SNARK and public inputs. `recipient` must equal the chain's
    /// configured `generic_call_wrapper_address`.
    pub proof: ProofDto,
    pub pub_inputs: PubInputsDto,
    pub aux: [OutputAuxDto; TRANSACT_OUT],
    pub generic: GenericBlob,
}

/// The calls, the outputs they must deliver and the refund note. Mirrors
/// `GenericCallWrapper.GenericArgs` minus the leg-1 proof. Everything but
/// `amount_in` is bound into `pubInputs.intentHash`.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GenericBlob {
    /// Decimal U256 string. Floor on what the withdraw delivers to the wrapper.
    pub amount_in: String,
    /// At most `MAX_CALLS`, each on the chain's `generic_allowed_calls`.
    pub calls: Vec<GenericCallDto>,
    /// Between one and `MAX_OUTPUTS`.
    pub outputs: Vec<GenericOutputDto>,
    /// Expiry in unix seconds, decimal. Past it the wrapper refunds into
    /// `refund_d` instead of making the calls.
    pub deadline: String,
    /// Decimal U256 string. Gas the wrapper must forward to the call leg; the
    /// relayer charges for it and bounds it by `generic_max_min_gas`.
    pub min_gas: String,
    /// Where the wrapper refunds a cancelled escrow, 0x-hex. Never zero, the
    /// wrapper or this relayer's Bundler.
    pub refund_to: String,
    /// Receives slippage cushions, unused input and native leftovers, 0x-hex.
    /// Bound as `refund_to` is.
    pub surplus_to: String,
    /// Deposit request for the refund note, which the wrapper escrows the
    /// unshield back into when the call leg fails. `payer` must equal the
    /// `generic_call_wrapper_address`.
    pub refund_d: DepositRequestDto,
    /// The refund deposit's own leaf.
    pub refund_aux_d: OutputAuxDto,
    /// The refund deposit's fee leaf.
    pub refund_fee_aux_d: OutputAuxDto,
}

/// Mirror of `CallExecutor.Call`.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GenericCallDto {
    pub target: String,
    /// Decimal U256 string, in wei.
    pub value: String,
    /// Calldata, 0x-hex. Its first four bytes are the selector the allowlist
    /// is checked against.
    pub data: String,
}

/// Mirror of `GenericCallWrapper.Output`.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GenericOutputDto {
    /// Decimal U256 string. Floor on what the calls deliver in the output
    /// token, in base units.
    pub min_out: String,
    /// Deposit request for the output note. `payer` must equal the
    /// `generic_call_wrapper_address`.
    pub deposit: DepositRequestDto,
    /// The output deposit's own leaf.
    pub aux: OutputAuxDto,
    /// The output deposit's fee leaf.
    pub fee_aux: OutputAuxDto,
}
