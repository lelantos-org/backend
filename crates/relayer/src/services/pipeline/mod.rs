//! Per-chain submission pipelines:
//!
//! - `SpendPipeline`, driven by `/v1/spend`. Reserves a `TRANSACT_OUT`-leaf slot,
//!   builds the matching batch witness, and calls `MASP.transfer`,
//!   `MASP.withdraw` or `NativeAdapter.withdrawNative`.
//! - `SwapPipeline`, driven by `/v1/swap`. The same witness as a spend, but the
//!   calldata targets `SwapWrapper.swap` and carries a leg-2 escrow blob
//!   alongside the leg-1 SNARK; the wrapper composes both legs in one
//!   transaction.
//! - `FlushPipeline`, driven by a timer. Pops pending escrowed deposits from the
//!   database and calls `flushBatch`.
//!
//! All three hold the per-chain `TreeMirror` mutex from reserve through submit
//! completion, so two concurrent submissions on a chain cannot interleave and
//! reorder on chain.

pub mod common;
pub mod deposit_failures;
pub mod deposit_preflight;
pub mod flush;
pub mod spend;
pub mod swap;

pub use flush::FlushPipeline;
pub use spend::{NativeRoute, SpendPipeline};
pub use swap::SwapPipeline;
