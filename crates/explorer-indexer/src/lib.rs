//! Explorer indexer: flow analytics for explorer-ui.
//!
//! Aggregates `asset_flows` and `yield_fee_events` from rows the ingester has
//! already written, and rebuilds the materialized views over them. Reads no
//! chain — everything needing an RPC, and every table the wallet or the relayer
//! depends on, belongs to `protocol-indexer`.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Owns one ticking service
//! (`ConsumeServiceImpl`) implementing `shared::tick::TickService`. Must not
//! depend on `crypto`, which is the privacy gate.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod repositories;
pub mod services;
