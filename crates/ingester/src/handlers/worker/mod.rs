//! One chain's worker: [`runner`] holds the chain lock and alternates catch-up
//! with the live tail, which [`live`] paces.

pub mod live;
pub mod runner;

pub use live::LiveExit;
pub use runner::{WorkerExit, run, run_inner};
