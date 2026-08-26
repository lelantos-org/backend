//! DefiLlama spot-price adapter.
//!
//! `GET {base_url}/prices/current/{chain}:{address},…` prices a batch of tokens
//! in one request, keyed by the coin string sent. Tokens the provider does not
//! know are absent from the response rather than reported as an error or a null
//! entry, so a missing key means "no price" and never "request failed".
//!
//! The payload carries `decimals` alongside the price, letting the flow
//! endpoints convert base units into USD without a separate ERC20 metadata
//! index.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

/// A token as the rest of the crate identifies it: chain id plus lowercase hex
/// address without a `0x` prefix, matching `hex::encode` of `assets.token`.
pub type TokenKey = (i64, String);

/// Chains DefiLlama can price, by EVM chain id. Anything absent, local anvil
/// included, is never sent upstream and has no price.
fn llama_chain(chain_id: i64) -> Option<&'static str> {
    Some(match chain_id {
        1 => "ethereum",
        10 => "optimism",
        137 => "polygon",
        8453 => "base",
        42161 => "arbitrum",
        43114 => "avax",
        _ => return None,
    })
}

/// DefiLlama grades every quote. Below this threshold a quote reflects thin
/// liquidity rather than a price and is discarded.
const MIN_CONFIDENCE: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenPrice {
    pub price_usd: f64,
    /// `None` when the provider priced the token but reported no decimals:
    /// sufficient for a spot price, insufficient to convert an amount.
    pub decimals: Option<u32>,
    /// Provider's own timestamp for the quote, not our fetch time.
    pub quoted_at: i64,
}

#[derive(Debug, Deserialize)]
struct PricesResponse {
    coins: HashMap<String, Coin>,
}

#[derive(Debug, Deserialize)]
struct Coin {
    price: f64,
    decimals: Option<u32>,
    timestamp: i64,
    #[serde(default)]
    confidence: Option<f64>,
}

pub struct PriceClient {
    http: reqwest::Client,
    base_url: String,
}

impl PriceClient {
    pub fn new(base_url: String, timeout: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("build price http client")?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// Price whichever of `tokens` the provider knows. Tokens on unsupported
    /// chains never leave the process, and an all-unsupported batch issues no
    /// request, keeping the local anvil stack offline.
    pub async fn fetch(&self, tokens: &[TokenKey]) -> Result<HashMap<TokenKey, TokenPrice>> {
        // Maps the coin string back to the key that requested it. The response
        // echoes the request verbatim, avoiding a reverse chain-slug map and any
        // assumption about the casing returned.
        let by_coin: HashMap<String, TokenKey> = tokens
            .iter()
            .filter_map(|k| Some((coin_id(llama_chain(k.0)?, &k.1), k.clone())))
            .collect();
        if by_coin.is_empty() {
            return Ok(HashMap::new());
        }

        let mut coins: Vec<&str> = by_coin.keys().map(String::as_str).collect();
        // Stable request URL so upstream caches and logs see one shape per token
        // set rather than one per HashMap iteration order.
        coins.sort_unstable();
        let url = format!("{}/prices/current/{}", self.base_url, coins.join(","));

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("GET {url}: status {status}");
        }
        let body: PricesResponse = resp.json().await.with_context(|| format!("decode {url}"))?;

        Ok(collect(body, &by_coin))
    }
}

fn coin_id(chain: &str, token_hex: &str) -> String {
    format!("{chain}:0x{}", token_hex.to_lowercase())
}

fn collect(
    body: PricesResponse,
    by_coin: &HashMap<String, TokenKey>,
) -> HashMap<TokenKey, TokenPrice> {
    body.coins
        .into_iter()
        .filter_map(|(coin, c)| {
            if c.price <= 0.0 || c.confidence.unwrap_or(1.0) < MIN_CONFIDENCE {
                return None;
            }
            let key = by_coin.get(&coin.to_lowercase())?;
            Some((
                key.clone(),
                TokenPrice {
                    price_usd: c.price,
                    decimals: c.decimals,
                    quoted_at: c.timestamp,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(chain_id: i64, hex: &str) -> TokenKey {
        (chain_id, hex.to_string())
    }

    fn by_coin(keys: &[TokenKey]) -> HashMap<String, TokenKey> {
        keys.iter()
            .filter_map(|k| Some((coin_id(llama_chain(k.0)?, &k.1), k.clone())))
            .collect()
    }

    fn parse(json: &str) -> PricesResponse {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn local_chains_have_no_provider_slug() {
        // Keeps the anvil stack from calling out.
        assert_eq!(llama_chain(31337), None);
        assert_eq!(llama_chain(1337), None);
        assert_eq!(llama_chain(1), Some("ethereum"));
    }

    #[test]
    fn coin_id_lowercases_and_prefixes() {
        assert_eq!(coin_id("ethereum", "A0B8"), "ethereum:0xa0b8");
    }

    #[test]
    fn maps_a_quote_back_to_its_token() {
        let k = key(1, "a0b8");
        let got = collect(
            parse(
                r#"{"coins":{"ethereum:0xa0b8":{"price":0.99,"decimals":6,"timestamp":42,"confidence":0.99}}}"#,
            ),
            &by_coin(std::slice::from_ref(&k)),
        );
        let p = got.get(&k).unwrap();
        assert_eq!(p.decimals, Some(6));
        assert_eq!(p.quoted_at, 42);
    }

    #[test]
    fn absent_tokens_yield_no_entry() {
        // The provider omits unknown tokens rather than erroring, so an empty
        // `coins` map is a valid "nothing is priced" answer.
        let k = key(1, "dead");
        assert!(collect(parse(r#"{"coins":{}}"#), &by_coin(&[k])).is_empty());
    }

    #[test]
    fn drops_low_confidence_and_non_positive_quotes() {
        let weak = key(1, "a0b8");
        let zero = key(1, "b1c9");
        let got = collect(
            parse(
                r#"{"coins":{
                    "ethereum:0xa0b8":{"price":1.0,"decimals":18,"timestamp":1,"confidence":0.1},
                    "ethereum:0xb1c9":{"price":0.0,"decimals":18,"timestamp":1,"confidence":0.99}
                }}"#,
            ),
            &by_coin(&[weak, zero]),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn keeps_a_quote_that_omits_confidence() {
        let k = key(1, "a0b8");
        let got = collect(
            parse(r#"{"coins":{"ethereum:0xa0b8":{"price":1.5,"decimals":18,"timestamp":1}}}"#),
            &by_coin(std::slice::from_ref(&k)),
        );
        assert_eq!(got.get(&k).unwrap().price_usd, 1.5);
    }

    /// Contract check against the live provider. Ignored by default because it
    /// needs the network. No fixture can catch this case: DefiLlama renaming a
    /// field or dropping `confidence` would leave every test above green while
    /// prices vanished in production.
    ///
    /// Run with `cargo test -p prices -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "hits the live DefiLlama API"]
    async fn the_live_provider_still_speaks_the_shape_we_parse() {
        let usdc = key(1, "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
        let client =
            PriceClient::new("https://coins.llama.fi".into(), Duration::from_secs(15)).unwrap();

        let got = client.fetch(std::slice::from_ref(&usdc)).await.unwrap();

        let p = got.get(&usdc).expect("USDC must be priced");
        assert_eq!(p.decimals, Some(6), "decimals field moved or vanished");
        assert!(
            p.price_usd > 0.5 && p.price_usd < 2.0,
            "implausible: {}",
            p.price_usd
        );
        assert!(
            p.quoted_at > 1_700_000_000,
            "timestamp field moved: {}",
            p.quoted_at
        );
    }

    #[test]
    fn a_priced_token_without_decimals_survives_but_cannot_convert() {
        let k = key(1, "a0b8");
        let got = collect(
            parse(r#"{"coins":{"ethereum:0xa0b8":{"price":2.0,"timestamp":1,"confidence":0.9}}}"#),
            &by_coin(std::slice::from_ref(&k)),
        );
        assert_eq!(got.get(&k).unwrap().decimals, None);
    }
}
