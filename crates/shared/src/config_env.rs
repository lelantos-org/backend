//! Reading config out of the environment.
//!
//! Two conventions live here. Chain-independent settings are plain variables
//! read by [`string`] and [`parse`]. Per-chain settings follow
//! `<PREFIX>_CHAIN_<chain_id>_<FIELD>` and are read by [`lookup`], so a binary's
//! TOML supplies static defaults while deployed addresses, signer keys and RPC
//! URLs come from the runtime environment, fed by docker-compose, k8s or shell
//! exports:
//!
//! ```sh
//! INGESTER_CHAIN_31337_POOL_ADDRESS=0xabc…
//! RELAYER_CHAIN_31337_SIGNER_KEY=0x59c6…
//! ```
//!
//! **An empty variable counts as unset**, uniformly, for every function here.
//! Compose and k8s both render an unset substitution as the empty string, so a
//! variable that arrived empty carries no operator intent; honouring it would
//! overwrite a good TOML value with nothing.

use std::str::FromStr;
use thiserror::Error;

/// The value of `key`, or `None` when it is unset or empty.
pub fn string(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// A variable that is set but does not parse as the type the field needs.
#[derive(Debug, Error)]
#[error("{key}={raw:?} is not a valid {expected}")]
pub struct ParseError {
    pub key: String,
    pub raw: String,
    /// The target type's name, from [`std::any::type_name`].
    pub expected: &'static str,
}

/// Read and parse `key`; `None` when it is unset or empty.
///
/// A malformed value is an error rather than a fallback to the default, so a
/// setting cannot read as set while behaving as unset — a typo'd
/// `CACHE_TTL_S=30s` must not serve the default TTL while the operator believes
/// it took effect.
///
/// ```
/// # unsafe { std::env::set_var("SHARED_DOC_TTL", "30") };
/// let ttl: u64 = shared::config_env::parse("SHARED_DOC_TTL").unwrap().unwrap_or(60);
/// assert_eq!(ttl, 30);
/// assert_eq!(shared::config_env::parse::<u64>("SHARED_DOC_UNSET").unwrap(), None);
/// ```
pub fn parse<T: FromStr>(key: &str) -> Result<Option<T>, ParseError> {
    let Some(raw) = string(key) else {
        return Ok(None);
    };
    raw.parse().map(Some).map_err(|_| ParseError {
        key: key.to_string(),
        raw,
        expected: std::any::type_name::<T>(),
    })
}

/// The value of `<PREFIX>_CHAIN_<chain_id>_<FIELD>`, or `None` when unset or
/// empty.
pub fn lookup(prefix: &str, chain_id: i64, field: &str) -> Option<String> {
    string(&format!("{prefix}_CHAIN_{chain_id}_{field}"))
}

/// [`lookup`], parsed. A malformed value is an error, as in [`parse`].
pub fn lookup_parse<T: FromStr>(
    prefix: &str,
    chain_id: i64,
    field: &str,
) -> Result<Option<T>, ParseError> {
    parse(&format!("{prefix}_CHAIN_{chain_id}_{field}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scopes a variable to one test.
    ///
    /// Every key below is unique to its test, so the mutations never race even
    /// though the harness runs them in parallel — which is what makes the
    /// `unsafe` sound. A shared key would not be.
    struct EnvVar(&'static str);

    impl EnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: this key is used by no other test, so no thread observes
            // the mutation.
            unsafe { std::env::set_var(key, value) };
            Self(key)
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe { std::env::remove_var(self.0) };
        }
    }

    #[test]
    fn test_string_reads_a_set_variable() {
        let _v = EnvVar::set("SHARED_TEST_STRING_SET", "value");
        assert_eq!(string("SHARED_TEST_STRING_SET"), Some("value".to_string()));
    }

    #[test]
    fn test_string_of_an_unset_variable_is_none() {
        assert_eq!(string("SHARED_TEST_STRING_MISSING"), None);
    }

    /// The rule the module doc states: compose renders an unset substitution as
    /// the empty string, and overlaying that would blank a good TOML value.
    #[test]
    fn test_an_empty_variable_reads_as_unset() {
        let _v = EnvVar::set("SHARED_TEST_STRING_EMPTY", "");
        assert_eq!(string("SHARED_TEST_STRING_EMPTY"), None);
        assert_eq!(parse::<u64>("SHARED_TEST_STRING_EMPTY").unwrap(), None);
    }

    #[test]
    fn test_parse_reads_a_set_variable() {
        let _v = EnvVar::set("SHARED_TEST_PARSE_OK", "42");
        assert_eq!(parse::<u64>("SHARED_TEST_PARSE_OK").unwrap(), Some(42));
    }

    /// The whole point of `parse` over an `and_then(|s| s.parse().ok())`: a
    /// value the operator set must never silently behave as the default.
    #[test]
    fn test_a_malformed_value_is_an_error_not_a_fallback() {
        let _v = EnvVar::set("SHARED_TEST_PARSE_BAD", "30s");
        let err = parse::<u64>("SHARED_TEST_PARSE_BAD").unwrap_err();
        assert_eq!(err.key, "SHARED_TEST_PARSE_BAD");
        assert_eq!(err.raw, "30s");
        // The message has to name the key and the value, or an operator cannot
        // tell which of a dozen variables refused to start the process.
        let msg = err.to_string();
        assert!(msg.contains("SHARED_TEST_PARSE_BAD"), "{msg}");
        assert!(msg.contains("30s"), "{msg}");
    }

    #[test]
    fn test_lookup_builds_the_per_chain_key() {
        let _v = EnvVar::set("SHARED_TEST_CHAIN_42_RPC_URL", "http://node");
        assert_eq!(
            lookup("SHARED_TEST", 42, "RPC_URL"),
            Some("http://node".to_string())
        );
        assert_eq!(lookup("SHARED_TEST", 43, "RPC_URL"), None);
    }

    #[test]
    fn test_lookup_parse_reads_the_per_chain_key() {
        let _v = EnvVar::set("SHARED_TEST_CHAIN_7_START_BLOCK", "1234");
        assert_eq!(
            lookup_parse::<i64>("SHARED_TEST", 7, "START_BLOCK").unwrap(),
            Some(1234)
        );
        let _bad = EnvVar::set("SHARED_TEST_CHAIN_8_START_BLOCK", "genesis");
        assert!(lookup_parse::<i64>("SHARED_TEST", 8, "START_BLOCK").is_err());
    }
}
