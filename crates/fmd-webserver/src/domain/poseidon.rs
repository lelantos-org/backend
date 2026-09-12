//! The Merkle leaf hash the commitment feed serves.

use crate::domain::error::{AppError, AppResult};
use common_crypto::tree::Field;

/// Compute the in-circuit Merkle leaf hash from `(cm, cv_dep_x, cv_dep_y)`.
///
/// Thin wrapper over [`common_crypto::tree::leaf_hash`], which owns the definition,
/// so that callers here keep working in `AppResult`.
pub fn leaf_hash(cm: &Field, cv_dep_x: &Field, cv_dep_y: &Field) -> AppResult<Field> {
    common_crypto::tree::leaf_hash(cm, cv_dep_x, cv_dep_y)
        .map_err(|e| AppError::Internal(e.to_string()))
}
