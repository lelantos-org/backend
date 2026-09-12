use ::asset_registry::AssetRow;
use serde::Serialize;

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

/// What one relayer reports about itself on a chain.
///
/// Deliberately holds nothing that describes the *deployment* — chain name,
/// browser RPC, explorer, Permit2, the contract addresses and the asset catalog
/// are registry-webserver's `/v1/chains` and `/v1/assets`, identical for every
/// relayer serving the chain. What is left is what only this relayer can answer:
/// the signer it will sign with, the pool it writes to, the tree it mirrors and
/// what it charges. A self-hosted relayer is therefore configured only with what
/// it operates.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainHealth {
    pub chain_id: i64,
    pub committed_count: i64,
    pub current_root_hex: String,
    /// EIP-55 checksummed MASP pool address.
    pub masp_address: String,
    /// True once a submission's outcome could not be determined. The mirror is
    /// parked, so the relayer rejects work on this chain until it restarts and
    /// `current_root_hex` may not match the chain.
    pub desynced: bool,
    /// EIP-55 checksummed relayer signer. Wallets bind this into the SNARK,
    /// and the pool rejects a proof naming anyone else.
    pub relayer_address: String,
    /// Depth of the tree this relayer actually mirrors — a reading, not a
    /// configured value.
    ///
    /// The second half of the wallet's cross-check, alongside `maspAddress`:
    /// registry-webserver publishes the depth the *deployment* declares, this
    /// publishes the depth this relayer will verify against, and a wallet that
    /// finds them different refuses the relayer rather than discovering the
    /// mismatch after building a proof. Taken from the compiled-in `tree::DEPTH`,
    /// so it cannot drift from the mirror it describes.
    pub tree_depth: u32,
    /// Shielded fee terms, when this relayer charges one.
    ///
    /// Presence means required: a client that sees this key must attach a fee
    /// output to every spend and swap, and one that does not must not. There is no
    /// separate `required` flag that could disagree with the key's presence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shielded_fee: Option<ShieldedFeeOut>,
}

/// What a client needs in order to pay this relayer privately.
///
/// Terms only, no amount. An amount is a function of the gas price and an oracle
/// rate, both of which move within the minute, while `/chains` is a boot registry
/// a wallet reads once and holds behind a 60s edge cache. The live number belongs
/// to `/v1/spend/estimate`, for the same reason registry-webserver publishes spot
/// prices on their own route rather than as a field on its catalog.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShieldedFeeOut {
    /// bech32m address to address the fee note to.
    pub address: String,
    /// How far below the relayer's submit-time quote a payment may fall and still
    /// be accepted. A quote is unsigned and re-derived when the spend arrives, so
    /// this is the drift allowed between the two.
    pub grace_bps: u32,
    /// Markup over raw gas cost, already included in every quoted amount.
    /// Published so a client can display what it is charged rather than recompute
    /// the amount.
    pub markup_bps: u32,
    /// Assets this relayer accepts as a fee.
    ///
    /// A wallet builds one spend in one asset, so this is also the list of assets
    /// the relayer will handle: an asset absent from here cannot pay for its own
    /// transfer.
    ///
    /// Repeated in full rather than named by id, so a client reading `shieldedFee`
    /// has the `scale` it needs to size the note without joining back to the
    /// deployment catalog registry-webserver publishes.
    pub tokens: Vec<TokenOut>,
}

/// One asset a wallet may hold on a chain.
///
/// Carries the label and decimals so a client can render an amount without a
/// per-token `symbol()` and `decimals()` round trip of its own.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenOut {
    /// MASP asset id, as used in circuit inputs.
    pub asset_id: i64,
    /// 0x-prefixed ERC-20 address.
    pub token: String,
    /// Circuit capacity parameter (`baseUnits / scale` must fit `uint48`), not a
    /// decimals normalizer. A decimal string, since it exceeds `u53`.
    pub scale: String,
    /// `null` until the indexer has read it: unknown, not 18.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decimals: Option<i16>,
    /// `null` until the indexer has read it, or when the token implements no
    /// `symbol()`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Protocol fee on a shield of this asset, in bps. Absent until an
    /// `AssetFeeSet` has been indexed.
    ///
    /// Rates are per asset and per leg — there is no pool-wide fee — so an
    /// absent value is unknown, not zero, and the two legs differ routinely.
    /// The deposit rate is charged **on top** of the principal, while the
    /// withdraw rate is **skimmed from** the proceeds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deposit_bps: Option<i16>,
    /// Protocol fee on an unshield of this asset, in bps. See `depositBps`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withdraw_bps: Option<i16>,
    /// Present iff this asset's custody earns in a venue. Absent means plain
    /// custody, where one circuit unit is worth `scale` base units forever.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yield_state: Option<YieldOut>,
}

/// What a yield-bearing asset is currently worth, and under what terms.
///
/// A client needs this to size a shield: the pull is
/// `ceil(units * gross / supply)`, and `Permit2.maxTotal` is signed over that
/// figure. Quoting at `scale` instead under-signs the allowance by whatever the
/// venue has earned, and the pull reverts.
///
/// `gross` and `supply` are published rather than only `index`, because that is
/// how the pool converts — `scale` and `RAY` cancel out of `units * gross /
/// supply`. Converting through the rounded index instead lands a unit or two
/// from what the contract charges, which at the boundary is the difference
/// between a pull that fits the signed ceiling and one that does not. `index` is
/// for display.
///
/// Absent from `TokenOut` entirely until the indexer's first poll lands: an
/// asset known to be yield-bearing but not yet priced must not be quoted at
/// `scale`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YieldOut {
    /// 0x-prefixed venue address. Bound once at registration and immutable.
    pub venue: String,
    /// Venue position plus the pool's idle balance, in base units.
    pub gross: String,
    /// Units outstanding, note holders plus the treasury's unswept fee.
    pub supply: String,
    /// `gross * RAY / (supply * scale)`, for display. Do not convert with it.
    pub index: String,
    /// The venue is no longer being supplied. Existing backing is unaffected —
    /// the asset degrades to zero-yield custody, still fully backed.
    pub halted: bool,
}

impl TokenOut {
    /// One registered asset, shaped for the wire.
    ///
    /// Carries no rate: what a venue has been paying is measured and published by
    /// registry-webserver, on the catalog it owns. A relayer restating it would be
    /// asserting a deployment-wide fact from a service a wallet cannot check it
    /// against — and a self-hosted one could state whatever it liked.
    pub fn new(a: &AssetRow) -> Self {
        Self {
            asset_id: a.asset_id_u64,
            token: format!("0x{}", hex::encode(&a.token)),
            scale: a.scale.to_string(),
            decimals: a.decimals,
            symbol: a.symbol.clone(),
            deposit_bps: a.deposit_bps,
            withdraw_bps: a.withdraw_bps,
            yield_state: yield_out(a),
        }
    }
}

/// `None` for a plain asset, and also for a yield asset the poller has not
/// reached yet — a client must not price the latter at `scale`.
fn yield_out(a: &AssetRow) -> Option<YieldOut> {
    let venue = a.venue.as_ref()?;
    let gross = a.gross.as_ref()?;
    let total = a.total_normalized.as_ref()?;
    let fee_units = a.accrued_fee_normalized.as_ref()?;
    Some(YieldOut {
        venue: format!("0x{}", hex::encode(venue)),
        gross: gross.to_string(),
        supply: (total + fee_units).to_string(),
        index: a.index_ray.as_ref()?.to_string(),
        halted: a.halted.unwrap_or(false),
    })
}

// `Clone` so one priced token can be fanned out into a quote per asset id
// registered at its address — see `FeeContext::quote`.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FeeQuote {
    pub token_symbol: String,
    pub token_address: String,
    pub decimals: u8,
    /// Base-unit U256 as decimal string.
    pub amount: String,
    /// MASP asset id, present once the indexer has registered this token.
    ///
    /// `null` means the relayer cannot yet map this fee token to an asset, so a
    /// client cannot build a fee note for it. It does not mean the token is
    /// unpriced; [`Self::amount`] is still meaningful for display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<i64>,
    /// `baseUnits = circuitUnits * scale`, decimal string. Absent alongside
    /// [`Self::asset_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    /// [`Self::amount`] rounded up to a whole circuit unit: the exact `value` to
    /// put in the fee note.
    ///
    /// Rounded here rather than by the client, since rounding down would underpay
    /// by up to one whole unit and be refused, and two implementations of the same
    /// rounding would drift. Absent alongside [`Self::asset_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circuit_amount: Option<String>,
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
    /// Where to send the fee note, when this chain collects a shielded fee.
    ///
    /// Absent means the relayer is not charging on this chain, and a spend with no
    /// fee output is still relayed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shielded_fee_address: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{ChainHealth, ShieldedFeeOut, TokenOut};

    fn health() -> ChainHealth {
        ChainHealth {
            chain_id: 31337,
            committed_count: 1,
            current_root_hex: "0xab".to_string(),
            masp_address: "0xMASP".to_string(),
            desynced: false,
            relayer_address: "0xRELAYER".to_string(),
            tree_depth: 11,
            shielded_fee: None,
        }
    }

    fn token(asset_id: i64) -> TokenOut {
        TokenOut {
            asset_id,
            token: "0xdead".to_string(),
            scale: "1000000000000".to_string(),
            decimals: Some(6),
            symbol: Some("USDC".to_string()),
            deposit_bps: Some(0),
            withdraw_bps: Some(20),
            yield_state: None,
        }
    }

    /// Presence of the key is what tells a client a fee is required, so a relayer
    /// that charges nothing must omit it entirely. A `null` would read as required
    /// with unknown terms.
    #[test]
    fn a_chain_that_charges_no_shielded_fee_omits_the_key_entirely() {
        let json = serde_json::to_value(health()).expect("serialize");
        let obj = json.as_object().expect("object");
        assert!(!obj.contains_key("shieldedFee"), "got {json}");
    }

    #[test]
    fn shielded_fee_terms_serialize_under_one_camel_case_key() {
        let mut h = health();
        h.shielded_fee = Some(ShieldedFeeOut {
            address: "lelantos1abc".to_string(),
            grace_bps: 300,
            markup_bps: 1000,
            tokens: vec![token(1)],
        });
        let json = serde_json::to_value(&h).expect("serialize");
        let fee = &json["shieldedFee"];
        assert_eq!(fee["address"], "lelantos1abc");
        assert_eq!(fee["graceBps"], 300);
        assert_eq!(fee["markupBps"], 1000);
        assert_eq!(fee["tokens"][0]["assetId"], 1);
        assert_eq!(fee["tokens"][0]["scale"], "1000000000000");
    }

    /// The deployment half moved to registry-webserver. A relayer that still
    /// published it would re-create the split this service exists to end, and
    /// would let a self-hosted relayer assert facts it has no authority over.
    #[test]
    fn test_serialize_chain_health_publishes_nothing_about_the_deployment() {
        let json = serde_json::to_value(health()).expect("serialize");

        for moved in [
            "chainName",
            "rpcUrl",
            "explorerUrl",
            "permit2Address",
            "nativeAdapterAddress",
            "swapWrapperAddress",
            "tokens",
        ] {
            assert!(
                json.get(moved).is_none(),
                "{moved} belongs to registry-webserver /v1/chains"
            );
        }
    }

    /// The readings only this relayer can make, and the two a wallet
    /// cross-checks against the registry. All unconditional: a relayer that
    /// cannot answer these has nothing a wallet can use.
    #[test]
    fn test_serialize_chain_health_carries_every_relayer_reading() {
        let json = serde_json::to_value(health()).expect("serialize");

        assert_eq!(json["chainId"], 31337);
        assert_eq!(json["relayerAddress"], "0xRELAYER");
        assert_eq!(json["committedCount"], 1);
        assert_eq!(json["currentRootHex"], "0xab");
        assert_eq!(json["desynced"], false);
        // The cross-check pair.
        assert_eq!(json["maspAddress"], "0xMASP");
        assert_eq!(json["treeDepth"], 11);
    }

    /// A fee token whose metadata the indexer has not resolved omits those
    /// fields rather than sending `null`, so a client can tell an unread symbol
    /// from a token that has none.
    #[test]
    fn test_serialize_token_omits_unresolved_metadata() {
        let mut h = health();
        h.shielded_fee = Some(ShieldedFeeOut {
            address: "lelantos1abc".to_string(),
            grace_bps: 300,
            markup_bps: 1000,
            tokens: vec![
                TokenOut {
                    symbol: Some("WETH".to_string()),
                    decimals: Some(18),
                    scale: "10000000000".to_string(),
                    ..token(1)
                },
                TokenOut {
                    asset_id: 2,
                    token: "0xdef".to_string(),
                    scale: "1".to_string(),
                    decimals: None,
                    symbol: None,
                    // Unindexed rates omit both keys, the same way unknown
                    // decimals and symbol do.
                    deposit_bps: None,
                    withdraw_bps: None,
                    yield_state: None,
                },
            ],
        });
        let json = serde_json::to_value(&h).expect("serialize");
        let tokens = &json["shieldedFee"]["tokens"];

        assert_eq!(tokens[0]["symbol"], "WETH");
        assert_eq!(tokens[0]["decimals"], 18);
        // Scale is a decimal string; it does not fit a JSON number safely.
        assert_eq!(tokens[0]["scale"], "10000000000");
        assert!(tokens[1].get("symbol").is_none());
        assert!(tokens[1].get("decimals").is_none());
        assert_eq!(tokens[1]["assetId"], 2);
    }
}
