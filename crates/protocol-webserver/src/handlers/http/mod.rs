//! The read path: every route this service answers.
//!
//! Stateless and identical on every replica — each handler is a `State`
//! extractor, a query struct and one call into `services`. Nothing here writes;
//! the one thing that does is [`crate::handlers::worker`].

pub mod assets;
pub mod chains;
pub mod governance;
pub mod health;
pub mod openapi;
pub mod prices;
pub mod router;
pub mod yield_index;

pub use assets::list_assets;
pub use chains::chains;
pub use governance::{get_proposal, list_proposals, list_votes};
pub use health::health;
pub use prices::prices;
pub use yield_index::yield_index;
