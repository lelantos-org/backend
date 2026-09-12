//! Talks to systems outside this process.
//!
//! Only one such system here: Postgres, as the holder of the per-chain
//! leadership locks. Everything else this binary touches goes through
//! `repositories`, which speaks rows rather than connections.

pub mod locks;

pub use locks::ChainLocks;
