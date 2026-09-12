//! Layer 3: pure types and the transforms over them — wire DTOs, the batch
//! shape, the Fiat-Shamir transcript, the error type and the response bodies.
//! No IO and no database.

pub mod batch;
pub mod deposit;
pub mod deposit_digest;
pub mod dto;
pub mod error;
pub mod fiat_shamir;
pub mod field;
pub mod responses;
pub mod shielded_address;
pub mod transact_pi;
pub mod units;
