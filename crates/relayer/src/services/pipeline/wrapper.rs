//! What the wrapper pipelines share: `swap/` submits through `SwapWrapper` and
//! `generic/` through `GenericCallWrapper`.
//!
//! Either wrapper calls `MASP.withdraw` for leg 1 and escrows deposits back into
//! the pool, so both pipelines bind leg 1 to the wrapper, check every escrowed
//! deposit, refuse receivers that cannot move funds and keep a margin before
//! the deadline.

use crate::adapters::abi::IMasp;
use crate::adapters::calldata::build_spend_tree;
use crate::adapters::parse::{FieldRef, parse_address, parse_field};
use crate::domain::dto::{DepositRequestDto, PubInputsDto};
use crate::domain::error::{AppError, AppResult};
use crate::services::pipeline::SUBMISSION_TIMEOUT;
use crate::services::pipeline::batcher;
use crate::services::pipeline::transact::TransactBinding;
use alloy::primitives::{Address, U256};
use std::time::{SystemTime, UNIX_EPOCH};

/// The addresses one chain's submissions through a wrapper are checked against.
#[derive(Debug, Clone, Copy)]
pub struct WrapperBinding {
    pub chain_id: i64,
    /// The wrapper contract. It calls `MASP.withdraw` for leg 1, so it is the
    /// pool's `msg.sender`: `pi_w.relayer`, `pi_w.recipient` and every escrowed
    /// deposit's `payer` must name it.
    pub wrapper: Address,
    /// This relayer's `Bundler`, the wrapper's caller, and so the address
    /// `pi_w.payer` must name.
    pub bundler: Address,
}

impl WrapperBinding {
    /// Leg 1 is a withdraw to the wrapper, proved with the wrapper as `relayer`.
    /// The transact SNARK enforces conservation; this only rejects a wrong shape
    /// before it costs a Groth16.
    pub fn check_leg1(&self, pi: &PubInputsDto) -> AppResult<()> {
        TransactBinding {
            chain_id: self.chain_id,
            relayer: self.wrapper,
        }
        .check(pi)?;
        if pi.public_out == 0 {
            return Err(AppError::BadRequest(
                "publicOut must be > 0: leg 1 withdraws to the wrapper".into(),
            ));
        }
        let recipient = parse_address(&pi.recipient)?;
        if recipient != self.wrapper {
            return Err(AppError::BadRequest(format!(
                "pi.recipient ({recipient}) must equal the wrapper ({})",
                self.wrapper
            )));
        }
        Ok(())
    }

    /// The Bundler calls the wrapper, which lets only `pi.payer` call it.
    pub fn check_payer(&self, pi: &PubInputsDto) -> AppResult<()> {
        let payer = parse_address(&pi.payer)?;
        if payer != self.bundler {
            return Err(AppError::BadRequest(format!(
                "pubInputs.payer ({payer}) must equal this relayer's submitter ({})",
                self.bundler
            )));
        }
        Ok(())
    }

    /// The shape every deposit a wrapper escrows shares: chain-bound on its own,
    /// paid for by the wrapper, and built into two leaves by the flush that
    /// materialises it. `name` is the deposit's field in the error.
    pub fn check_deposit(&self, d: &DepositRequestDto, name: &str) -> AppResult<()> {
        // It rides in the same calldata as leg 1, but the wrapper escrows it into
        // MASP under its own `chainId` field.
        if d.chain_id != self.chain_id as u64 {
            return Err(AppError::BadRequest(format!(
                "{name}.chainId ({}) must equal the request chainId ({})",
                d.chain_id, self.chain_id
            )));
        }
        let payer = parse_address(&d.payer)?;
        if payer != self.wrapper {
            return Err(AppError::BadRequest(format!(
                "{name}.payer ({payer}) must equal the wrapper ({})",
                self.wrapper
            )));
        }
        if d.public_in == 0 {
            return Err(AppError::BadRequest(format!("{name}.publicIn must be > 0")));
        }
        // The flush that materialises it takes `inner` and `feeInner` as batch
        // coefficients and reverts `CoefficientOutOfField` on a non-canonical one,
        // which would leave the escrow unflushable.
        let field = |suffix: &str| format!("{name}.{suffix}");
        parse_field(&d.inner, FieldRef::Named(&field("inner")))?;
        parse_field(&d.fee_inner, FieldRef::Named(&field("feeInner")))?;
        Ok(())
    }

    /// Refuses a `receiver` that [`receiver_error`] names. `field` is the
    /// receiver's field in the error.
    pub fn check_receiver(&self, receiver: Address, field: &str) -> AppResult<()> {
        match receiver_error(receiver, Some(self.wrapper), self.bundler) {
            Some(why) => Err(AppError::BadRequest(format!("{field}: {why}"))),
            None => Ok(()),
        }
    }
}

/// Why `receiver` cannot be paid by `wrapper`, if it cannot: the wrapper reverts
/// on zero or itself, and the Bundler has no way to move what it receives.
/// Shared by request validation and the boot check on the refund address this
/// relayer advertises.
pub fn receiver_error(
    receiver: Address,
    wrapper: Option<Address>,
    bundler: Address,
) -> Option<String> {
    [
        (Some(Address::ZERO), "the zero address"),
        (Some(bundler), "this relayer's Bundler"),
        (wrapper, "the wrapper"),
    ]
    .into_iter()
    .find(|(forbidden, _)| *forbidden == Some(receiver))
    .map(|(_, what)| format!("{receiver} is {what}, which cannot move what it receives"))
}

/// How long before its deadline a submission is still accepted. One accepted
/// closer to it could be proved and bundled only after the deadline, and the
/// wrapper would then refund it, with the wallet's fees paid. The longest a
/// caller waits for a submission, since one still queued past that is already a
/// slow bundle.
pub const DEADLINE_MARGIN_SECS: u64 = SUBMISSION_TIMEOUT.as_secs();

/// Refuses a `deadline` less than [`DEADLINE_MARGIN_SECS`] away. `field` names
/// it in the error.
pub fn check_deadline(deadline: U256, field: &str) -> AppResult<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if deadline < U256::from(now + DEADLINE_MARGIN_SECS) {
        return Err(AppError::BadRequest(format!(
            "{field} ({deadline}) is less than {DEADLINE_MARGIN_SECS}s away; the \
             submission could land after it and be refunded instead"
        )));
    }
    Ok(())
}

/// `tp_w` and `tpi_w` for args built before a slot is reserved. The batcher
/// replaces both at encode.
pub fn placeholder_tree_update() -> (IMasp::Proof, IMasp::SpendTree) {
    (
        batcher::zero_proof(),
        build_spend_tree(0, &[0u8; 32], 0, U256::ZERO),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::pipeline::fixtures::{binding, bundler, wrapper};

    #[test]
    fn check_receiver_refuses_zero_bundler_or_wrapper() {
        let binding = binding();
        binding
            .check_receiver(Address::repeat_byte(0x33), "swap.refundTo")
            .unwrap();
        for bad in [Address::ZERO, bundler(), wrapper()] {
            let err = binding.check_receiver(bad, "swap.refundTo").unwrap_err();
            assert!(
                matches!(&err, AppError::BadRequest(m) if m.starts_with("swap.refundTo: ")),
                "{bad}: {err}"
            );
        }
    }
}
