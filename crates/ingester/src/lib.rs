//! Block-log ingester.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Per-chain workers do
//! backfill (catch-up) then live-tail. Live-tail is `LiveServiceImpl` in
//! `services::live`; the worker handler just calls `tick()` on a schedule.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;
pub mod version;
