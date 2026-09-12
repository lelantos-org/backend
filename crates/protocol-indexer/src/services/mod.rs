//! The two ticking services this binary runs, one module each.
//!
//! [`consume`] projects `raw_events` into the four tables this crate owns;
//! [`yield_state`] polls the chain for what no event carries. Both implement
//! `shared::tick::TickService` and share one pool, deliberately in one process:
//! the poller's `UPDATE` only matches rows `consume` has already created.

pub mod consume;
pub mod yield_state;
