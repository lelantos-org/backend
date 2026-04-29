//! Standard config-loading helper.
//!
//! Every binary follows the same shape: env var holds a path, default path
//! used when unset, file is TOML deserialized into the binary's config
//! struct. Use [`load_toml`] to avoid reinventing it.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use std::path::PathBuf;

pub fn load_toml<T: DeserializeOwned>(path_env: &str, default_path: &str) -> Result<T> {
    let path = std::env::var(path_env).unwrap_or_else(|_| default_path.to_string());
    let txt = std::fs::read_to_string(PathBuf::from(&path))
        .with_context(|| format!("reading {}", path))?;
    toml::from_str(&txt).with_context(|| format!("parsing {}", path))
}
