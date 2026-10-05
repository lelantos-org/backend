pub mod deposit;
pub mod estimate;
pub mod generic;
pub mod swap;
pub mod transact;

pub use deposit::DepositRequestDto;
pub use estimate::{
    EstimateDepositRequest, EstimateGenericRequest, EstimateSpendRequest, EstimateSwapRequest,
};
pub use generic::{GenericBlob, GenericCallDto, GenericOutputDto, SubmitGenericPayload};
pub use swap::{SubmitSwapPayload, SwapBlob};
pub use transact::{
    OutputAuxDto, PointDto, ProofDto, PubInputsDto, SpendKind, SubmitSpendPayload, TRANSACT_IN,
    TRANSACT_OUT,
};

#[cfg(test)]
mod wire_contract;
