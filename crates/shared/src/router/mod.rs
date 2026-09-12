//! Router-level building blocks shared by the webserver crates.
//!
//! Separate from [`crate::http`], which owns the error mapping, and from
//! [`crate::request_span`], which owns the trace span. What lives here is the
//! plumbing a router assembles around its own routes:
//!
//!   - [`cache_control`] and friends: the freshness policy, per route.
//!   - [`etag`]: conditional GET, for the routes worth revalidating.
//!   - [`service_layers`]: the deadline, body cap, tracing and metrics every
//!     service wraps its routes in.

mod cache_control;
mod etag;
mod layers;

pub use cache_control::{cache_control, cache_control_value, public_max_age};
pub use etag::etag;
pub use layers::{Limits, service_layers};
