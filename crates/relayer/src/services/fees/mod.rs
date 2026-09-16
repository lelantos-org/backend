//! What a relayed operation costs, and how it is paid for.
//!
//! - `gas_witness`: gas units per entry point, learned from receipts.
//! - `gas_estimator`: what a unit of gas costs now.
//! - `oracle`: native-to-fee-token prices.
//! - `quote`: the three combined into a per-token fee quote.
//! - `shielded/`: recognising and pricing the fee notes a payer attaches to a
//!   spend, swap or deposit.

pub mod gas_estimator;
pub mod gas_witness;
pub mod oracle;
pub mod quote;
pub mod shielded;
