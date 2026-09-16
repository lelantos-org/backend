//! Layer 5: orchestration. Everything that combines repositories and adapters
//! into one of the relayer's jobs:
//!
//! - `tree/`: the per-chain commitment-tree mirror.
//! - `admission/`: nullifier reservation and idempotent replay, before a
//!   submission reaches a pipeline.
//! - `transact_verifier/`: local verification of a wallet's transact proof.
//! - `fees/`: quoting gas in fee tokens, and collecting shielded fees.
//! - `pipeline/`: the spend, swap and flush pipelines and the per-chain batcher
//!   that proves and submits their operations.
//! - `submitter`, `witness` and `events`: sending transactions, building the
//!   tree-update witness and publishing deposit lifecycle events.

pub mod admission;
pub mod events;
pub mod fees;
pub mod pipeline;
pub mod submitter;
pub mod transact_verifier;
pub mod tree;
pub mod witness;
