//! Entry points, one module per shape.
//!
//! [`http`] is the read path — stateless, identical on every replica. [`worker`]
//! is the write path — one elected process per chain.
pub mod http;
pub mod worker;
