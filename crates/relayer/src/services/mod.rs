//! Layer 5: orchestration. Everything that combines repositories and adapters
//! into one of the relayer's jobs — mirroring the tree, quoting and collecting
//! fees, admitting submissions, and the three pipelines in `pipeline/` that
//! prove and submit.

pub mod asset_registry;
pub mod deposit_fee;
pub mod deposit_mempool;
pub mod escrow;
pub mod events;
pub mod fee_quote;
pub mod gas_estimator;
pub mod gas_witness;
pub mod idempotency;
pub mod nullifier_guard;
pub mod oracle;
pub mod pipeline;
pub mod shielded_fee;
pub mod submitter;
pub mod transact_verifier;
pub mod tree;
pub mod witness;
