use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayerSubmitResponse {
    /// Tx hash returned once the on-chain `transact()` call confirms.
    pub tx_hash: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    /// Crate version from `Cargo.toml`.
    pub version: &'static str,
    /// Short git commit SHA at build time, or `"unknown"` outside a repo.
    pub commit: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainsResponse {
    pub chains: Vec<ChainHealth>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainHealth {
    pub chain_id: i64,
    pub committed_count: i64,
    pub current_root_hex: String,
    /// EIP-55 checksummed MASP pool address.
    pub masp_address: String,
    /// True once a submission's outcome could not be determined. The mirror is
    /// parked: the relayer rejects work on this chain until it restarts, and
    /// `current_root_hex` may no longer match the chain.
    pub desynced: bool,
    /// EIP-55 checksummed relayer signer. Wallets bind this into the SNARK,
    /// and the pool rejects a proof naming anyone else.
    pub relayer_address: String,
    /// Flattened into the same JSON object: a client sees one flat chain
    /// record, while the split keeps the live mirror readings above separate
    /// from the static configuration below.
    #[serde(flatten)]
    pub config: ChainConfigOut,
    /// Assets registered on this chain, lowest id first.
    ///
    /// Empty when the indexer has not caught up, which a client must read as
    /// "not known yet" rather than "this chain supports nothing".
    pub tokens: Vec<TokenOut>,
}

/// One asset a wallet may hold on a chain.
///
/// Carries the label and decimals so a client can render an amount without a
/// per-token `symbol()` / `decimals()` round trip of its own — reads that used
/// to happen once per asset on every load, and whose silent failure left the
/// wrapped-native token unidentifiable.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenOut {
    /// MASP asset id, as used in circuit inputs.
    pub asset_id: i64,
    /// 0x-prefixed ERC-20 address.
    pub token: String,
    /// Circuit capacity parameter (`baseUnits / scale` must fit `uint48`), NOT
    /// a decimals normalizer. Decimal string: it exceeds `u53`.
    pub scale: String,
    /// `null` until the indexer has read it. Unknown, never assume 18.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decimals: Option<i16>,
    /// `null` until the indexer has read it, or when the token implements no
    /// `symbol()`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

/// Spot USD prices for the registered assets, across every chain.
///
/// A separate route rather than a field on [`TokenOut`] because the two have
/// nothing in common but their key: `/chains` is a boot registry a client reads
/// once and holds, while a price is stale within the minute. Bolting one onto
/// the other would mean either refetching the registry to move a price or
/// showing a price fixed at page load.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricesResponse {
    /// Shared with the response cache rather than copied out of it. `serde`'s
    /// `rc` feature is on workspace-wide, which is what lets this serialize in
    /// place — the same shape `explorer-webserver`'s handlers return.
    pub prices: Arc<Vec<PriceOut>>,
}

/// One priced token.
///
/// A token the provider does not know — a local test token, or any token on a
/// chain it does not cover — is **absent** from `prices` rather than carried
/// with a zero. Absence means "unknown", and a client must render nothing
/// rather than `$0.00`, which would read as a measurement.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceOut {
    pub chain_id: i64,
    /// 0x-prefixed ERC-20 address, spelled exactly as [`TokenOut::token`] so a
    /// client can join the two without normalising either.
    pub token: String,
    /// Spot USD price of one whole token.
    pub price_usd: f64,
    /// Provider's own timestamp for the quote, not our fetch time, so a client
    /// can age it rather than trusting it indefinitely.
    pub price_at: i64,
}

impl From<crate::repositories::assets::AssetRow> for TokenOut {
    fn from(a: crate::repositories::assets::AssetRow) -> Self {
        Self {
            asset_id: a.asset_id_u64,
            token: format!("0x{}", hex::encode(&a.token)),
            scale: a.scale.to_string(),
            decimals: a.decimals,
            symbol: a.symbol,
        }
    }
}

/// The configuration half of a chain record: static, deployment-supplied, and
/// absent field by field until an operator fills it in.
///
/// Every field is optional and omitted when unset rather than serialised as
/// `null`, so a client can tell "this deployment does not describe it" from
/// "it is described as empty" and fall back to its own defaults.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainConfigOut {
    /// EIP-55 checksummed `NativeAdapter`, when one is deployed. Absent means
    /// native-coin deposit and withdraw have no entry point on this chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_adapter_address: Option<String>,
    /// EIP-55 checksummed `SwapWrapper`, when one is deployed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap_wrapper_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_name: Option<String>,
    /// Browser-reachable RPC; not the relayer's own endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permit2_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explorer_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeQuote {
    pub token_symbol: String,
    pub token_address: String,
    pub decimals: u8,
    /// Base-unit U256 as decimal string.
    pub amount: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateResponse {
    pub gas_used: u64,
    pub effective_gas_price_wei: String,
    pub total_native_wei: String,
    /// Per-chain markup applied (bps; 1000 = 10%).
    pub markup_bps: u32,
    /// Unix seconds (server time) when this quote was produced.
    pub quoted_at: u64,
    pub fees: Vec<FeeQuote>,
}

#[cfg(test)]
mod tests {
    use super::{ChainConfigOut, ChainHealth, TokenOut};

    fn health(config: ChainConfigOut) -> ChainHealth {
        health_with(config, vec![])
    }

    fn health_with(config: ChainConfigOut, tokens: Vec<TokenOut>) -> ChainHealth {
        ChainHealth {
            chain_id: 31337,
            committed_count: 1,
            current_root_hex: "0xab".to_string(),
            masp_address: "0xMASP".to_string(),
            desynced: false,
            relayer_address: "0xRELAYER".to_string(),
            config,
            tokens,
        }
    }

    /// The config half is `#[serde(flatten)]`ed, so a client sees one flat
    /// record. Nesting it would silently break every consumer's parser.
    #[test]
    fn test_serialize_chain_health_flattens_config_into_one_object() {
        let json = serde_json::to_value(health(ChainConfigOut {
            chain_name: Some("anvil".to_string()),
            rpc_url: Some("http://localhost:8545".to_string()),
            tree_depth: Some(10),
            ..Default::default()
        }))
        .expect("serialize");

        assert_eq!(json["chainId"], 31337);
        assert_eq!(json["relayerAddress"], "0xRELAYER");
        // Flat, not `json["config"]["chainName"]`.
        assert_eq!(json["chainName"], "anvil");
        assert_eq!(json["rpcUrl"], "http://localhost:8545");
        assert_eq!(json["treeDepth"], 10);
        assert!(json.get("config").is_none());
    }

    /// An undescribed field is omitted rather than serialised as `null`.
    ///
    /// That distinction is load-bearing for the wallet: absent means "this
    /// deployment does not describe it, use your own default", which is how a
    /// relayer predating the registry keeps existing clients working.
    #[test]
    fn test_serialize_chain_health_omits_undescribed_config_fields() {
        let json = serde_json::to_value(health(ChainConfigOut::default())).expect("serialize");

        for absent in [
            "chainName",
            "rpcUrl",
            "treeDepth",
            "permit2Address",
            "nativeAdapterAddress",
            "swapWrapperAddress",
            "explorerUrl",
        ] {
            assert!(json.get(absent).is_none(), "{absent} must be omitted");
        }
        // The live readings are unconditional and must survive the omission.
        assert_eq!(json["committedCount"], 1);
        assert_eq!(json["maspAddress"], "0xMASP");
    }

    /// `tokens` is always present, so a client can tell "no assets indexed
    /// yet" from "this relayer predates the field" — the first is an empty
    /// array, the second is a missing key.
    #[test]
    fn test_serialize_chain_health_always_carries_a_token_array() {
        let json = serde_json::to_value(health(ChainConfigOut::default())).expect("serialize");
        assert_eq!(json["tokens"], serde_json::json!([]));
    }

    /// A token whose metadata the indexer has not resolved omits those fields
    /// rather than sending `null`, matching how the config half behaves.
    #[test]
    fn test_serialize_token_omits_unresolved_metadata() {
        let json = serde_json::to_value(health_with(
            ChainConfigOut::default(),
            vec![
                TokenOut {
                    asset_id: 1,
                    token: "0xabc".to_string(),
                    scale: "10000000000".to_string(),
                    decimals: Some(18),
                    symbol: Some("WETH".to_string()),
                },
                TokenOut {
                    asset_id: 2,
                    token: "0xdef".to_string(),
                    scale: "1".to_string(),
                    decimals: None,
                    symbol: None,
                },
            ],
        ))
        .expect("serialize");

        assert_eq!(json["tokens"][0]["symbol"], "WETH");
        assert_eq!(json["tokens"][0]["decimals"], 18);
        // Scale is a decimal string: it does not fit a JSON number safely.
        assert_eq!(json["tokens"][0]["scale"], "10000000000");
        assert!(json["tokens"][1].get("symbol").is_none());
        assert!(json["tokens"][1].get("decimals").is_none());
        assert_eq!(json["tokens"][1]["assetId"], 2);
    }
}
