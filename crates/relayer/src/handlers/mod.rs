//! Layer 6: entry points. HTTP only; the flush worker's tick loop is driven
//! from `app::state` rather than from here.

pub mod http;
