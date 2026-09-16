use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    /// Crate version from `Cargo.toml`.
    pub version: &'static str,
    /// Short git commit SHA at build time, or `"unknown"` outside a repo.
    pub commit: &'static str,
}
