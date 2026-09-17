use super::*;
use alloy::primitives::address;

const TOML_MASP: Address = address!("1111111111111111111111111111111111111111");
const TOML_PERMIT2: Address = address!("2222222222222222222222222222222222222222");
const ENV_MASP: &str = "0x3333333333333333333333333333333333333333";

/// Scopes a variable to one test. Every key below is unique to its test, so
/// the mutations never race even though tests run in parallel — which is
/// what makes the `unsafe` sound.
struct EnvVar(&'static str);

impl EnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        // SAFETY: this key is used by no other test.
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

fn chain(chain_id: u64) -> ChainCfg {
    ChainCfg {
        chain_id,
        upstream_url: "http://toml:8545".into(),
        upstream_archive: true,
        fallback_url: None,
        fallback_archive: false,
        reorg_depth: 64,
        deploy_block: None,
        upstream_units_per_second: 300,
        upstream_burst_units: 1_500,
        masp_address: TOML_MASP,
        permit2_address: TOML_PERMIT2,
        erc20_seed: vec![],
        venue_seed: vec![],
        governor_address: None,
        gov_token_address: None,
    }
}

fn cfg(chains: Vec<ChainCfg>) -> RpcProxyConfig {
    RpcProxyConfig {
        listen_addr: "0.0.0.0:3006".into(),
        metrics_addr: default_metrics_addr(),
        trusted_client_ip_header: None,
        trusted_client_ip_position: HeaderPosition::default(),
        max_batch: 100,
        max_log_range: 5_000,
        max_call_data_bytes: 8 * 1024,
        max_call_gas: 50_000_000,
        upstream_max_inflight: default_upstream_max_inflight(),
        rate_limit: RateLimitCfg::default(),
        chains,
    }
}

#[test]
fn overlay_replaces_a_declared_chains_fields() {
    let _u = EnvVar::set("RPC_PROXY_CHAIN_881001_UPSTREAM_URL", "http://env:8545");
    let _m = EnvVar::set("RPC_PROXY_CHAIN_881001_MASP_ADDRESS", ENV_MASP);

    let mut c = cfg(vec![chain(881001)]);
    c.apply_env_overlay().unwrap();

    assert_eq!(c.chains[0].upstream_url, "http://env:8545");
    assert_eq!(
        c.chains[0].masp_address,
        ENV_MASP.parse::<Address>().unwrap()
    );
    // Untouched fields keep the TOML value.
    assert_eq!(c.chains[0].permit2_address, TOML_PERMIT2);
}

/// The overlaid value is an API key or a contract address. Retaining the
/// TOML value on a parse failure would serve traffic that differs from the
/// configuration the operator believes is in effect.
#[test]
fn a_malformed_address_fails_startup_rather_than_falling_back() {
    let _m = EnvVar::set("RPC_PROXY_CHAIN_881002_MASP_ADDRESS", "0xdeadbeef");

    let err = cfg(vec![chain(881002)]).apply_env_overlay().unwrap_err();
    assert!(
        err.to_string()
            .contains("RPC_PROXY_CHAIN_881002_MASP_ADDRESS"),
        "{err}"
    );
    assert!(err.to_string().contains("0xdeadbeef"), "{err}");
}

/// Compose renders an unset substitution as the empty string; honouring it
/// would blank a good TOML value — here, the upstream URL, which would take
/// the chain down.
#[test]
fn an_empty_variable_reads_as_unset() {
    let _u = EnvVar::set("RPC_PROXY_CHAIN_881003_UPSTREAM_URL", "");
    let _m = EnvVar::set("RPC_PROXY_CHAIN_881003_MASP_ADDRESS", "");

    let mut c = cfg(vec![chain(881003)]);
    c.apply_env_overlay().unwrap();

    assert_eq!(c.chains[0].upstream_url, "http://toml:8545");
    assert_eq!(c.chains[0].masp_address, TOML_MASP);
}

#[test]
fn a_seed_list_is_replaced_wholesale_and_validated() {
    let a = "0x4444444444444444444444444444444444444444";
    let b = "0x5555555555555555555555555555555555555555";
    let _e = EnvVar::set("RPC_PROXY_CHAIN_881004_ERC20_SEED", &format!("{a}, {b}"));

    let mut c = cfg(vec![chain(881004)]);
    c.chains[0].erc20_seed = vec![TOML_MASP];
    c.apply_env_overlay().unwrap();

    assert_eq!(
        c.chains[0].erc20_seed,
        vec![a.parse::<Address>().unwrap(), b.parse().unwrap()],
        "the env value replaces the TOML list rather than adding to it"
    );
}

/// One bad entry must not be dropped silently: the address it names is a
/// token whose balance would stop loading, with nothing to say why.
#[test]
fn a_malformed_seed_entry_fails_startup() {
    let _e = EnvVar::set(
        "RPC_PROXY_CHAIN_881005_ERC20_SEED",
        "0x4444444444444444444444444444444444444444,0xnope",
    );
    let err = cfg(vec![chain(881005)]).apply_env_overlay().unwrap_err();
    assert!(err.to_string().contains("0xnope"), "{err}");
}

#[test]
fn overlay_supplies_the_governance_addresses_and_zero_reads_as_absent() {
    let gov = "0x7777777777777777777777777777777777777777";
    let _g = EnvVar::set("RPC_PROXY_CHAIN_881006_GOVERNOR_ADDRESS", gov);
    let _t = EnvVar::set(
        "RPC_PROXY_CHAIN_881006_GOV_TOKEN_ADDRESS",
        "0x0000000000000000000000000000000000000000",
    );

    let mut c = cfg(vec![chain(881006)]);
    c.apply_env_overlay().unwrap();

    assert_eq!(c.chains[0].governor(), Some(gov.parse().unwrap()));
    assert_eq!(
        c.chains[0].gov_token(),
        None,
        "zero is the TOML placeholder"
    );
}

#[test]
fn a_malformed_governor_address_fails_startup() {
    let _g = EnvVar::set("RPC_PROXY_CHAIN_881007_GOVERNOR_ADDRESS", "0xnope");
    let err = cfg(vec![chain(881007)]).apply_env_overlay().unwrap_err();
    assert!(
        err.to_string()
            .contains("RPC_PROXY_CHAIN_881007_GOVERNOR_ADDRESS"),
        "{err}"
    );
}

#[test]
fn a_duplicate_chain_block_is_rejected() {
    let err = cfg(vec![chain(1), chain(1)]).validate().unwrap_err();
    assert!(err.to_string().contains("declared more than once"), "{err}");
}

#[test]
fn an_empty_chain_list_is_rejected() {
    assert!(cfg(vec![]).validate().is_err());
}

#[test]
fn an_empty_upstream_url_is_rejected() {
    let mut c = chain(1);
    c.upstream_url = "  ".into();
    assert!(cfg(vec![c]).validate().is_err());
}

/// Which class an address lands in decides which functions it exposes, so
/// a generator that emitted it twice must not be resolved by coin flip.
#[test]
fn an_address_in_two_seeds_is_rejected() {
    let dup: Address = "0x4444444444444444444444444444444444444444"
        .parse()
        .unwrap();
    let mut c = chain(1);
    c.erc20_seed = vec![dup];
    c.venue_seed = vec![dup];
    assert!(cfg(vec![c]).validate().is_err());
}

/// The overlay is what usually introduces a bad ceiling, and a too-large one
/// fails silently: it starts, and removes the check it exists to be.
#[test]
fn a_cost_ceiling_outside_its_range_is_rejected() {
    for mutate in [
        (|c: &mut RpcProxyConfig| c.max_batch = 0) as fn(&mut RpcProxyConfig),
        |c| c.max_batch = 100_000,
        |c| c.max_log_range = 0,
        |c| c.max_log_range = u64::MAX,
        |c| c.max_call_data_bytes = 10 * 1024 * 1024,
        |c| c.max_call_gas = u64::MAX,
        |c| c.upstream_max_inflight = 0,
        |c| c.upstream_max_inflight = 100_000,
    ] {
        let mut c = cfg(vec![chain(1)]);
        mutate(&mut c);
        assert!(c.validate().is_err(), "must reject");
    }

    assert!(cfg(vec![chain(1)]).validate().is_ok(), "defaults are valid");
}

/// A chain with no archive endpoint anywhere still starts — it degrades the
/// earned column, which is today's behaviour, rather than failing to boot.
#[test]
fn archive_availability_is_reported_not_enforced() {
    let mut c = chain(1);
    c.upstream_archive = false;
    assert!(!c.has_archive());
    assert!(cfg(vec![c.clone()]).validate().is_ok());

    c.fallback_url = Some("http://fallback:8545".into());
    c.fallback_archive = true;
    assert!(c.has_archive());
}
