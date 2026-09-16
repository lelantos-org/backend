//! One chain's worker: [`runner`] holds the chain lock and alternates catch-up
//! with the live tail, which [`live`] paces; [`supervisor`] restarts it.

pub mod live;
pub mod runner;
pub mod supervisor;

pub use runner::{WorkerExit, run};
