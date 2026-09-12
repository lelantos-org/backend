//! Pure types and the decode/transform helpers that move rows onto the wire.
//! No IO and no database access. See `backend/ARCHITECTURE.md`.
//!
//! The one exception is [`responses::chunks`], which names axum to implement
//! `IntoResponse` for a body the services render rather than the handlers;
//! that module explains why it cannot live a layer up.

pub mod dto;
pub mod error;
pub mod field;
pub mod poseidon;
pub mod responses;
pub mod token;
