//! Talking to the chain.
//!
//! Only the venue-APY measurement reaches outside this process; the HTTP routes
//! answer from the database and the caches in front of it.

pub mod rpc;
pub mod venue;
