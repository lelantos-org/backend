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
//! All three hand their operation to the chain's `batcher`, which reserves,
//! proves and sends up to `bundle_max_items` of them in one `Bundler.execute`
//! transaction, chaining each tree update on the previous one's root.

use std::time::Duration;

/// The longest a caller waits for a submission.
///
/// A submission waits in the chain's batcher until its bundle lands, so a caller
/// can wait behind the bundle in flight, its proofs and its receipt. Sized above
/// one submission's worst case, two `receipt_timeout_s` windows of 60 s each by
/// default plus a proof, so it trips on a stuck server rather than a busy one.
pub const SUBMISSION_TIMEOUT: Duration = Duration::from_secs(180);

pub mod batcher;
pub mod flush;
pub mod spend;
pub mod swap;
pub mod transact;

pub use flush::FlushPipeline;
pub use spend::SpendPipeline;
pub use swap::SwapPipeline;
