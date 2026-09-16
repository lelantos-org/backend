//! Orchestration over the repositories and the RPC adapter.
//!
//! The two ingestion modes are [`live`] (tail the head, one block-range tick at
//! a time) and [`backfill`] (chunked, parallel catch-up). They share their whole
//! read side — [`log_range::fetch_rows`] — and their whole write side —
//! [`ingest::IngestService`] — so the two cannot drift on windowing, decoding or
//! how the cursor moves. What differs is the pacing and what an empty range
//! means, and that is all each module holds.

pub mod backfill;
pub mod ingest;
pub mod live;
pub mod log_range;
pub mod reorg;
pub mod retry;
