//! Deployment registry, asset catalog and spot prices.
//!
//! The half of what a wallet used to fetch from the relayer that is a property of
//! the *deployment* rather than of any one relayer: which chains exist, what is
//! deployed on them, which assets are registered, what they are worth, and what
//! a yield venue has been paying.
//!
//! Split out so a relayer can be self-hosted to broadcast one operator's own
//! transactions without also being the authority on a chain's explorer URL — and
//! so this, which is stateless and read-only, scales freely while a relayer
//! cannot.
//!
//! The two halves are the module layout. `handlers::http` is the read path:
//! stateless, identical on every replica, answering from the database and the
//! caches in front of it. `handlers::worker` is the write path: the venue-APY
//! measurement, which elects one replica per chain through a Postgres advisory
//! lock and stores what it measures, so the replica that measures need not be
//! the one that answers.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::build_info;
pub use app::{AppState, RegistryConfig, build_state};
pub use handlers::http::router::build as build_router;
