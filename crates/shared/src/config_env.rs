// Per-chain config env overlay. Lets each binary's TOML supply static
// defaults while the dynamic bits (deployed addresses, signer keys, RPC
// URLs) come from the runtime environment — fed by docker-compose, k8s,
// or shell exports.
//
// Convention: `<PREFIX>_CHAIN_<chain_id>_<FIELD>`, e.g.
//   INGESTER_CHAIN_31337_POOL_ADDRESS=0xabc…
//   RELAYER_CHAIN_31337_SIGNER_KEY=0x59c6…
//
// Returns Some(value) iff the env var is set and non-empty.

pub fn lookup(prefix: &str, chain_id: i64, field: &str) -> Option<String> {
    let key = format!("{}_CHAIN_{}_{}", prefix, chain_id, field);
    match std::env::var(&key) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SAFETY: tests run sequentially within this module; no other thread
    // observes the per-test mutations.
    #[test]
    fn missing_returns_none() {
        unsafe { std::env::remove_var("UNIT_TEST_CHAIN_42_X") };
        assert_eq!(lookup("UNIT_TEST", 42, "X"), None);
    }

    #[test]
    fn set_returns_some() {
        unsafe { std::env::set_var("UNIT_TEST_CHAIN_42_Y", "value") };
        assert_eq!(lookup("UNIT_TEST", 42, "Y"), Some("value".to_string()));
        unsafe { std::env::remove_var("UNIT_TEST_CHAIN_42_Y") };
    }

    #[test]
    fn empty_returns_none() {
        unsafe { std::env::set_var("UNIT_TEST_CHAIN_42_Z", "") };
        assert_eq!(lookup("UNIT_TEST", 42, "Z"), None);
        unsafe { std::env::remove_var("UNIT_TEST_CHAIN_42_Z") };
    }
}
