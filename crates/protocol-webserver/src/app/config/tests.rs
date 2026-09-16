use super::*;

fn chain(chain_id: i64) -> ChainCfg {
    ChainCfg {
        chain_id,
        name: None,
        rpc_url: None,
        read_rpc_url: None,
        explorer_url: None,
        permit2_address: None,
        masp_address: None,
        tree_depth: None,
        native_adapter_address: None,
        swap_wrapper_address: None,
        apy_rpc_url: None,
    }
}

fn cfg(chains: Vec<ChainCfg>) -> RegistryConfig {
    RegistryConfig {
        database_url: "postgres://localhost/x".into(),
        bind_addr: default_bind_addr(),
        metrics_addr: default_metrics_addr(),
        cache_ttl_s: default_cache_ttl(),
        token_prices: TokenPricesCfg::default(),
        chains,
    }
}

#[test]
fn test_a_described_deployment_validates() {
    assert!(cfg(vec![chain(31337), chain(31338)]).validate().is_ok());
}

/// Every route here answers from the chain list. With none, the service
/// would boot and serve an empty registry, which reads to a wallet exactly
/// like a deployment that supports nothing.
#[test]
fn test_no_chains_is_rejected() {
    let err = cfg(vec![]).validate().expect_err("must reject");
    assert!(format!("{err}").contains("no chains"), "{err}");
}

/// The overlay keys on chain id, so a duplicate would make
/// `REGISTRY_CHAIN_<id>_*` silently apply to only one of the two.
#[test]
fn test_duplicate_chain_id_is_rejected() {
    let err = cfg(vec![chain(31337), chain(31337)])
        .validate()
        .expect_err("must reject");
    assert!(format!("{err}").contains("31337"), "{err}");
}

#[test]
fn test_empty_database_url_is_rejected() {
    let mut c = cfg(vec![chain(31337)]);
    c.database_url = String::new();
    assert!(c.validate().is_err());
}
