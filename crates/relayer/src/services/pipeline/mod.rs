// Per-chain submission pipelines. Three flavours:
//
//   * `SpendPipeline`  — HTTP-driven (`/v1/spend`). Reserves a
//                        `TRANSACT_OUT`-leaf slot, builds the matching batch
//                        witness, calls `MASP.transfer` / `MASP.withdraw`, or
//                        `NativeAdapter.withdrawNative`.
//   * `SwapPipeline`   — HTTP-driven (`/v1/swap`). Same witness as a spend,
//                        but calldata targets `SwapWrapper.swap` and carries
//                        a leg-2 escrow blob alongside the leg-1 SNARK; the
//                        wrapper composes both legs in a single tx.
//   * `FlushPipeline`  — cron-driven. Pops pending escrowed deposits from the
//                        DB — one leaf each — and calls `flushBatch`.
//
// All three hold the per-chain `TreeMirror` mutex from reserve through
// submit completion. That serialization gates against intra-chain races:
// two concurrent submissions cannot interleave and reorder on chain.

pub mod common;
pub mod deposit_failures;
pub mod deposit_preflight;
pub mod flush;
pub mod spend;
pub mod swap;

pub use flush::FlushPipeline;
pub use spend::{NativeRoute, SpendPipeline};
pub use swap::SwapPipeline;
