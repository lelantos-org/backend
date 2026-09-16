//! The write path: background workers, elected rather than replicated.
//!
//! The HTTP half of this service ([`crate::handlers::http`]) is stateless and
//! answers from any replica. What lives here is the opposite — work that must
//! happen exactly once per chain, whichever replica ends up doing it.
pub mod venue_apy;
