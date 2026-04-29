// Per-chain submission pipelines. Three flavours:
//
//   * `SpendPipeline`  — HTTP-driven (`/v1/spend`). Reserves a 2-leaf slot,
//                        builds N=1 batch witness, calls `transfer` /
//                        `withdraw` / `withdrawNative`.
//   * `SwapPipeline`   — HTTP-driven (`/v1/swap`). Same N=1 witness as a
//                        spend, but calldata targets `SwapWrapper.swap`
//                        and carries a leg-2 escrow blob alongside the
//                        leg-1 SNARK; the wrapper composes both legs in a
//                        single tx.
//   * `FlushPipeline`  — cron-driven. Pops pending escrow intents from the
//                        DB, builds N-batch witness, calls `flushBatch`.
//
// All three hold the per-chain `TreeMirror` mutex from reserve through
// submit completion. That serialization gates against intra-chain races:
// two concurrent submissions cannot interleave and reorder on chain.

pub mod common;
pub mod flush;
pub mod spend;
pub mod swap;

pub use flush::FlushPipeline;
pub use spend::SpendPipeline;
pub use swap::SwapPipeline;
