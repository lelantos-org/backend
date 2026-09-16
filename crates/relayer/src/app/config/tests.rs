//! Config validation and the per-chain env overlay.

use super::*;

fn chain(chain_id: i64) -> ChainCfg {
    ChainCfg {
        chain_id,
        rpc_url: "http://localhost:8545".into(),
        pool_address: "0x0000000000000000000000000000000000000001".into(),
        bundler_address: "0x0000000000000000000000000000000000000002".into(),
        bundle_max_items: default_bundle_max_items(),
        bundle_linger_ms: 0,
        max_tx_bytes: default_max_tx_bytes(),
        signer_key_hex: "0x01".into(),
        refund_address: None,
        receipt_timeout_s: default_receipt_timeout_s(),
        receipt_poll_interval_ms: default_receipt_poll_interval_ms(),
        flush_interval_s: default_flush_interval_s(),
        flush_max_n: default_flush_max_n(),
        flush_max_attempts: default_flush_max_attempts(),
        flush_partial_after_s: 0,
        native_adapter_address: None,
        swap_wrapper_address: None,
        native_symbol: default_native_symbol(),
        native_decimals: default_native_decimals(),
        fee_markup_bps: default_fee_markup_bps(),
        accepted_fee_tokens: vec![],
        shielded_fee_address: None,
        shielded_fee_ivk: None,
        shielded_fee_grace_bps: default_shielded_fee_grace_bps(),
        shielded_fee_assets: vec![],
    }
}

#[test]
fn a_shielded_fee_address_without_its_viewing_key_is_refused() {
    let mut c = chain(1);
    c.shielded_fee_address = Some("lelantos1abc".into());
    let err = cfg(vec![c]).validate().expect_err("half a config");
    assert!(err.to_string().contains("must be set together"), "{err}");
}

#[test]
fn a_viewing_key_without_an_address_is_refused() {
    let mut c = chain(1);
    c.shielded_fee_ivk = Some("0x01".into());
    assert!(cfg(vec![c]).validate().is_err());
}

/// A 100% grace band clears any fee, including none, so it looks like
/// enforcement without being it.
#[test]
fn a_grace_band_of_a_whole_is_refused() {
    let mut c = chain(1);
    c.shielded_fee_grace_bps = BPS_DENOMINATOR;
    let err = cfg(vec![c]).validate().expect_err("grace of 100%");
    assert!(err.to_string().contains("shielded_fee_grace_bps"), "{err}");
}

#[test]
fn accepted_assets_without_an_address_collect_nothing_and_are_refused() {
    let mut c = chain(1);
    c.shielded_fee_assets = vec![1];
    assert!(cfg(vec![c]).validate().is_err());
}

#[test]
fn a_complete_shielded_fee_block_validates() {
    let mut c = chain(1);
    c.shielded_fee_address = Some("lelantos1abc".into());
    c.shielded_fee_ivk = Some("0x01".into());
    c.shielded_fee_assets = vec![1, 3];
    assert!(cfg(vec![c]).validate().is_ok());
}

fn cfg(chains: Vec<ChainCfg>) -> RelayerConfig {
    RelayerConfig {
        database_url: "postgres://localhost/x".into(),
        listen_addr: "0.0.0.0:3003".into(),
        chains,
        prover: ProverCfg {
            graph_path: "/g".into(),
            zkey_path: "/z".into(),
            transact_vkey_path: None,
        },
        price_oracle: PriceOracleCfg::default(),
        test_hooks: TestHooksCfg::default(),
    }
}

#[test]
fn a_well_formed_config_validates() {
    cfg(vec![chain(1), chain(2)]).validate().unwrap();
}

#[test]
fn bundle_size_is_bounded() {
    for bad in [0, MAX_BUNDLE_ITEMS + 1] {
        let mut c = chain(1);
        c.bundle_max_items = bad;
        let err = cfg(vec![c]).validate().expect_err("out of range");
        assert!(err.to_string().contains("bundle_max_items"), "{err}");
    }
}

/// Bundling stubs the verifiers out of its dry run, so an invalid wallet proof
/// must be caught locally first.
#[test]
fn bundling_requires_the_transact_verification_key() {
    let mut c = chain(1);
    c.bundle_max_items = 4;
    let err = cfg(vec![c.clone()]).validate().expect_err("no vkey");
    assert!(err.to_string().contains("transact_vkey_path"), "{err}");

    let mut with_key = cfg(vec![c]);
    with_key.prover.transact_vkey_path = Some("/vk.json".into());
    with_key.validate().unwrap();
}

#[test]
fn a_size_cap_too_small_for_one_swap_is_refused() {
    let mut c = chain(1);
    c.max_tx_bytes = 4_000;
    let err = cfg(vec![c]).validate().expect_err("too small");
    assert!(err.to_string().contains("max_tx_bytes"), "{err}");
}

/// A duplicate would build two independent `TreeMirror`s and two flush workers
/// for one chain, which desyncs rather than merely misconfigures.
#[test]
fn a_duplicate_chain_id_is_refused() {
    let err = cfg(vec![chain(1), chain(1)]).validate().unwrap_err();
    assert!(err.to_string().contains("more than once"), "got {err}");
}

/// An operator fixing a config sees every mistake at once.
#[test]
fn every_problem_is_reported_not_just_the_first() {
    let mut c = chain(1);
    c.native_decimals = 39;
    c.fee_markup_bps = u32::MAX;
    c.flush_interval_s = 0;

    let err = cfg(vec![c]).validate().unwrap_err().to_string();

    assert!(err.contains("native_decimals"), "got {err}");
    assert!(err.contains("fee_markup_bps"), "got {err}");
    assert!(err.contains("flush_interval_s"), "got {err}");
}

#[test]
fn decimals_that_would_overflow_the_fee_math_are_refused() {
    let mut c = chain(1);
    c.native_decimals = 39;
    assert!(cfg(vec![c]).validate().is_err());

    let mut c = chain(1);
    c.accepted_fee_tokens.push(FeeTokenCfg {
        symbol: "X".into(),
        address: "0x0000000000000000000000000000000000000002".into(),
        decimals: 77,
        quote_symbol: "USDC".into(),
    });
    assert!(cfg(vec![c]).validate().is_err());
}

#[test]
fn an_absurd_markup_is_refused() {
    let mut c = chain(1);
    c.fee_markup_bps = u32::MAX;
    assert!(cfg(vec![c]).validate().is_err());
}

#[test]
fn a_zero_flush_interval_is_refused() {
    let mut c = chain(1);
    c.flush_interval_s = 0;
    assert!(cfg(vec![c]).validate().is_err());
}

/// The overlay lets a deploy script inject ERC-20 addresses it only learns at
/// deploy time. A chain id no other test touches keeps the process-wide env
/// mutation from reaching them.
///
/// SAFETY: the mutations are scoped to a chain id used nowhere else, so no
/// concurrent test observes them.
#[test]
fn test_accepted_fee_tokens_env_overlay_replaces_the_toml_list() {
    const CHAIN: i64 = 987_654;
    let key = format!("RELAYER_CHAIN_{CHAIN}_ACCEPTED_FEE_TOKENS");
    unsafe {
        std::env::set_var(
            &key,
            r#"[{"symbol":"USDC","address":"0xabc","decimals":6,"quote_symbol":"USD"}]"#,
        );
    }

    let mut config = cfg(vec![ChainCfg {
        accepted_fee_tokens: vec![FeeTokenCfg {
            symbol: "STALE".into(),
            address: "0xstale".into(),
            decimals: 18,
            quote_symbol: "USD".into(),
        }],
        ..chain(CHAIN)
    }]);
    config.apply_env_overlay();

    unsafe { std::env::remove_var(&key) };

    let tokens = &config.chains[0].accepted_fee_tokens;
    assert_eq!(
        tokens.len(),
        1,
        "the TOML entry must be replaced, not merged"
    );
    assert_eq!(tokens[0].symbol, "USDC");
    assert_eq!(tokens[0].address, "0xabc");
    assert_eq!(tokens[0].decimals, 6);
}

/// Absent means keep what the TOML declared: the overlay is optional, and a
/// deployment configuring fee tokens statically must keep working.
#[test]
fn test_accepted_fee_tokens_without_the_env_var_keeps_the_toml_list() {
    const CHAIN: i64 = 987_655;
    unsafe { std::env::remove_var(format!("RELAYER_CHAIN_{CHAIN}_ACCEPTED_FEE_TOKENS")) };

    let mut config = cfg(vec![ChainCfg {
        accepted_fee_tokens: vec![FeeTokenCfg {
            symbol: "KEEP".into(),
            address: "0xkeep".into(),
            decimals: 18,
            quote_symbol: "USD".into(),
        }],
        ..chain(CHAIN)
    }]);
    config.apply_env_overlay();

    assert_eq!(config.chains[0].accepted_fee_tokens[0].symbol, "KEEP");
}

/// A numeric setting that does not parse fails boot rather than leaving the
/// TOML's in force unnoticed.
///
/// SAFETY: as above, the chain id is used nowhere else, so the variable left
/// set by the panic reaches no other test.
#[test]
#[should_panic(expected = "BUNDLE_MAX_ITEMS")]
fn test_malformed_numeric_env_fails_boot() {
    const CHAIN: i64 = 987_656;
    unsafe { std::env::set_var(format!("RELAYER_CHAIN_{CHAIN}_BUNDLE_MAX_ITEMS"), "eight") };
    cfg(vec![chain(CHAIN)]).apply_env_overlay();
}
