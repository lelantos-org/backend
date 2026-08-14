//! Internal address screening API.
//!
//! Answers "is this address banned / high risk?" from the
//! `screened_addresses` table. Read-only by design: there is no write
//! endpoint, so network reach to this service cannot be used to *remove* a
//! sanctioned address, which is what makes running it unauthenticated behind
//! the gateway acceptable. The list is populated out-of-band by SQL.
//!
//! Screening is **fail-closed**: if the table cannot be read the request
//! fails with 500 rather than reporting the address as clean. Callers choose
//! their own posture from there.

pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::build_info;
pub use app::{AppState, RiskWebserverConfig, build_state};
pub use handlers::http::router::build as build_router;
