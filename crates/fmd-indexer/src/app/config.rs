use crate::domain::error::{FmdIndexerError, Result};
use serde::Deserialize;
use std::path::Path;

const DEFAULT_PATH: &str = "fmd-indexer.toml";
const ENV_PATH: &str = "FMD_INDEXER_CONFIG";
const ENV_DATABASE_URL: &str = "DATABASE_URL";

#[derive(Debug, Clone, Deserialize)]
pub struct FmdIndexerConfig {
    pub database_url: String,
    #[serde(default = "default_filter_workers")]
    pub filter_workers: usize,
    #[serde(default = "default_filter_batch")]
    pub filter_batch: usize,
    #[serde(default = "default_filter_tick_ms")]
    pub filter_tick_ms: u64,
    /// Consume-loop batch size. Defaults to `filter_batch`, but the two loops
    /// price it differently: for the filter it is a throughput knob, while for
    /// consume it bounds the widest tx that can be committed at all.
    #[serde(default)]
    pub consume_batch: Option<usize>,
    #[serde(default)]
    pub consume_tick_ms: Option<u64>,
    /// Where `/metrics` is served. This is the process's only listener.
    /// Defaults to loopback; under compose it is set to `0.0.0.0:<port>` and
    /// the host publish restricts it (see `shared::metrics::init`).
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
}

impl FmdIndexerConfig {
    pub fn consume_batch(&self) -> usize {
        self.consume_batch.unwrap_or(self.filter_batch)
    }
    pub fn consume_tick_ms(&self) -> u64 {
        self.consume_tick_ms.unwrap_or(self.filter_tick_ms)
    }
}

impl FmdIndexerConfig {
    pub fn load() -> Result<Self> {
        let path = std::env::var(ENV_PATH).unwrap_or_else(|_| DEFAULT_PATH.to_string());
        let mut cfg = if Path::new(&path).exists() {
            toml::from_str(&std::fs::read_to_string(&path)?)?
        } else {
            Self {
                database_url: std::env::var(ENV_DATABASE_URL)
                    .map_err(|_| FmdIndexerError::Config("DATABASE_URL not set".into()))?,
                filter_workers: default_filter_workers(),
                filter_batch: default_filter_batch(),
                filter_tick_ms: default_filter_tick_ms(),
                consume_batch: None,
                consume_tick_ms: None,
                metrics_addr: default_metrics_addr(),
            }
        };
        cfg.apply_env_overlay()?;
        Ok(cfg)
    }

    /// Overlay env vars on whichever base was loaded.
    ///
    /// Applied to the TOML branch too, not just the fallback: this used to be
    /// either/or, so the `FILTER_*` vars compose sets were read on neither path
    /// — there is no `fmd-indexer.toml` in the repo, and the env branch only
    /// looked at `DATABASE_URL` and `METRICS_ADDR`. Every tuning knob the
    /// deployment exposed was silently inert.
    fn apply_env_overlay(&mut self) -> Result<()> {
        if let Ok(v) = std::env::var(ENV_DATABASE_URL) {
            self.database_url = v;
        }
        if let Ok(v) = std::env::var("METRICS_ADDR") {
            self.metrics_addr = v;
        }
        if let Some(v) = parse_env("FILTER_WORKERS")? {
            self.filter_workers = v;
        }
        if let Some(v) = parse_env("FILTER_BATCH")? {
            self.filter_batch = v;
        }
        if let Some(v) = parse_env("FILTER_TICK_MS")? {
            self.filter_tick_ms = v;
        }
        if let Some(v) = parse_env("CONSUME_BATCH")? {
            self.consume_batch = Some(v);
        }
        if let Some(v) = parse_env("CONSUME_TICK_MS")? {
            self.consume_tick_ms = Some(v);
        }
        Ok(())
    }
}

/// Read and parse `key`, or `None` when it is unset.
///
/// A typo is an error rather than a fallback to the default: a knob that reads
/// as set and behaves as unset is the exact failure this overlay exists to fix.
fn parse_env<T: std::str::FromStr>(key: &str) -> Result<Option<T>> {
    let Ok(raw) = std::env::var(key) else {
        return Ok(None);
    };
    raw.parse()
        .map(Some)
        .map_err(|_| FmdIndexerError::Config(format!("{key}={raw:?} is not valid")))
}

fn default_metrics_addr() -> String {
    "127.0.0.1:3012".into()
}

fn default_filter_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}
fn default_filter_batch() -> usize {
    1000
}
fn default_filter_tick_ms() -> u64 {
    500
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// The process environment is global, and cargo runs these on threads of
    /// one process — without this, one test's `FILTER_BATCH` is visible to
    /// another's overlay call.
    static ENV: Mutex<()> = Mutex::new(());

    /// Sets `key` for the duration of the guard, serialised against every other
    /// test in this module.
    struct EnvVar {
        /// Held for the guard's lifetime, never read.
        _guard: MutexGuard<'static, ()>,
        key: &'static str,
    }

    impl EnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: `ENV` serialises every mutation and read in this module,
            // and nothing else in the process touches these keys.
            unsafe { std::env::set_var(key, value) };
            Self { _guard: guard, key }
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            unsafe { std::env::remove_var(self.key) };
        }
    }

    fn base() -> FmdIndexerConfig {
        FmdIndexerConfig {
            database_url: "postgres://localhost/db".into(),
            filter_workers: default_filter_workers(),
            filter_batch: default_filter_batch(),
            filter_tick_ms: default_filter_tick_ms(),
            consume_batch: None,
            consume_tick_ms: None,
            metrics_addr: default_metrics_addr(),
        }
    }

    /// The regression this overlay exists for: compose set `FILTER_TICK_MS` and
    /// nothing read it, on either load path.
    #[test]
    fn test_apply_env_overlay_with_filter_tick_ms_set_overrides_the_default() {
        let _env = EnvVar::set("FILTER_TICK_MS", "50");
        let mut cfg = base();
        cfg.apply_env_overlay().expect("overlay");
        assert_eq!(cfg.filter_tick_ms, 50);
    }

    /// A malformed value must stop the process, not silently keep the default —
    /// a knob that reads as set and behaves as unset is the original bug.
    #[test]
    fn test_apply_env_overlay_with_unparsable_value_returns_error() {
        let _env = EnvVar::set("FILTER_BATCH", "lots");
        let mut cfg = base();
        assert!(matches!(
            cfg.apply_env_overlay(),
            Err(FmdIndexerError::Config(_))
        ));
    }

    /// `consume_*` stay `None` unless explicitly set: `None` is what makes them
    /// track `filter_*` rather than freeze a copy of it.
    #[test]
    fn test_apply_env_overlay_with_no_consume_vars_leaves_them_unset() {
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = base();
        cfg.apply_env_overlay().expect("overlay");
        assert_eq!(cfg.consume_batch, None);
        assert_eq!(cfg.consume_tick_ms, None);
    }
}
