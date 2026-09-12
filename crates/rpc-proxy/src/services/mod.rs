pub mod housekeeping;
pub mod proxy;

/// Kept at its original path; [`Tip`](crate::domain::tip::Tip) is a pure type
/// and lives in `domain/`.
pub mod tip {
    pub use crate::domain::tip::*;
}
