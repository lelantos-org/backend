//! Cache keys and the canonicalisation they depend on.
//!
//! Two clients asking the same question must produce the same key. Spelling
//! varies: viem sends lowercase addresses and minimal quantities, other tooling
//! sends EIP-55 checksummed addresses and zero-padded ones. Without
//! normalisation the cache remains correct but stops hitting, with no visible
//! failure.
//!
//! Over-normalising is worse. A quantity may drop leading zeros, since `0x01a`
//! and `0x1a` name one block; calldata may not, since `0x0a` and `0xa` are
//! different byte strings and collapsing them would serve one call's answer for
//! another. Normalisation is therefore applied per method at known parameter
//! positions, never by walking the JSON and inferring what a string means.

use crate::domain::allowlist::Method;
use crate::domain::blocktag::BlockTag;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// Identifies one cached answer.
///
/// The params are hashed rather than held: `eth_call` calldata and
/// `eth_getLogs` topic arrays are unbounded, and a key that embedded them would
/// make the cache's memory a function of what callers send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub chain_id: u64,
    pub method: Method,
    digest: [u8; 32],
}

/// The key for one request.
pub fn key(chain_id: u64, method: Method, params: &[Value]) -> CacheKey {
    let canon = canonical(method, params);
    let mut h = Sha256::new();
    // The chain id and method are hashed as well as held in the struct, so a
    // digest is distinguishing on its own if compared outside a `CacheKey`.
    h.update(chain_id.to_be_bytes());
    h.update(method.label().as_bytes());
    h.update(serde_json::to_vec(&canon).expect("canonical form is serializable"));
    CacheKey {
        chain_id,
        method,
        digest: h.finalize().into(),
    }
}

/// The normalised parameter form a key is computed from.
///
/// Every arm rebuilds the params explicitly rather than editing them in place,
/// so a field nobody normalised cannot ride along and split the key space.
fn canonical(method: Method, params: &[Value]) -> Value {
    match method {
        // No parameters to normalise. `eth_chainId` never reaches a cache at
        // all, being answered from config.
        Method::EthChainId | Method::EthBlockNumber => Value::Null,

        Method::EthGetBalance => json!([
            lower(params.first()),
            BlockTag::parse(params.get(1)).canonical(),
        ]),

        Method::EthCall => {
            let obj = params.first().and_then(Value::as_object);
            let mut out = Map::new();
            // Only the fields that change the answer. A client sending
            // `gasPrice` or `nonce` on a read gets the same entry as one that
            // does not, which is correct: neither affects an `eth_call` result.
            out.insert("to".into(), json!(field_lower(obj, "to")));
            out.insert("from".into(), json!(field_lower(obj, "from")));
            // Both spellings of the calldata field name the same bytes.
            let data = obj
                .and_then(|o| o.get("data").or_else(|| o.get("input")))
                .and_then(Value::as_str)
                .unwrap_or("");
            // Lowercased but NOT length-normalised: these are bytes, and
            // `0x0a` is not `0xa`.
            out.insert("data".into(), json!(data.to_ascii_lowercase()));
            out.insert("value".into(), json!(field_lower(obj, "value")));
            json!([
                Value::Object(out),
                BlockTag::parse(params.get(1)).canonical()
            ])
        }

        Method::EthGetLogs => {
            let f = params.first().and_then(Value::as_object);
            let mut out = Map::new();
            // The single-address and one-entry-array forms mean the same query
            // and must share an entry; the allowlist has already refused
            // anything wider.
            let address = f
                .and_then(|o| o.get("address"))
                .map(|v| match v {
                    Value::Array(a) => lower(a.first()),
                    other => lower(Some(other)),
                })
                .unwrap_or_default();
            out.insert("address".into(), json!(address));
            out.insert(
                "fromBlock".into(),
                json!(BlockTag::parse(f.and_then(|o| o.get("fromBlock"))).canonical()),
            );
            out.insert(
                "toBlock".into(),
                json!(BlockTag::parse(f.and_then(|o| o.get("toBlock"))).canonical()),
            );
            // Topics are 32-byte values, so like calldata they are lowercased
            // and never shortened. Position and nesting are preserved: `null`
            // in a topic slot is a wildcard, and an array is an "any of" set.
            out.insert(
                "topics".into(),
                f.and_then(|o| o.get("topics"))
                    .map(lower_deep)
                    .unwrap_or(Value::Null),
            );
            json!([Value::Object(out)])
        }

        Method::EthGetBlockByNumber => json!([
            BlockTag::parse(params.first()).canonical(),
            params.get(1).and_then(Value::as_bool).unwrap_or(false),
        ]),

        Method::EthGetBlockByHash => json!([
            lower(params.first()),
            params.get(1).and_then(Value::as_bool).unwrap_or(false),
        ]),

        Method::EthGetTransactionReceipt | Method::EthGetTransactionByHash => {
            json!([lower(params.first())])
        }
    }
}

/// A string param, lowercased. Absent or non-string reads as empty, which the
/// allowlist has already refused for every method that reaches here.
fn lower(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").to_ascii_lowercase()
}

fn field_lower(obj: Option<&Map<String, Value>>, k: &str) -> String {
    lower(obj.and_then(|o| o.get(k)))
}

/// Lowercase every string in a tree, preserving its shape.
///
/// Used only for topics, whose nesting is meaningful. Safe there because every
/// leaf is a hex hash where case carries no information.
fn lower_deep(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(s.to_ascii_lowercase()),
        Value::Array(a) => Value::Array(a.iter().map(lower_deep).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAIN: u64 = 42161;
    const LOWER: &str = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";
    /// The same address, EIP-55 checksummed, as a wallet or explorer writes it.
    const CHECKSUMMED: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

    fn k(method: Method, params: serde_json::Value) -> CacheKey {
        let p: Vec<Value> = serde_json::from_value(params).unwrap();
        key(CHAIN, method, &p)
    }

    /// The canonicalisation the hit ratio depends on: one question, one key.
    #[test]
    fn a_checksummed_and_a_lowercase_address_share_a_key() {
        assert_eq!(
            k(
                Method::EthGetBalance,
                serde_json::json!([CHECKSUMMED, "latest"])
            ),
            k(Method::EthGetBalance, serde_json::json!([LOWER, "latest"]))
        );
    }

    #[test]
    fn two_spellings_of_one_height_share_a_key() {
        assert_eq!(
            k(
                Method::EthGetBlockByNumber,
                serde_json::json!(["0x01a", false])
            ),
            k(
                Method::EthGetBlockByNumber,
                serde_json::json!(["0x1a", false])
            )
        );
    }

    /// viem omits the trailing block tag on some reads and sends `"latest"` on
    /// others. Both ask the same question.
    #[test]
    fn an_omitted_block_tag_matches_an_explicit_latest() {
        assert_eq!(
            k(
                Method::EthCall,
                serde_json::json!([{"to": LOWER, "data": "0x70a08231"}])
            ),
            k(
                Method::EthCall,
                serde_json::json!([{"to": LOWER, "data": "0x70a08231"}, "latest"])
            )
        );
    }

    /// The counterpart, and the one that must NOT collapse: calldata is bytes.
    /// Normalising it as a quantity would serve one call's answer for another.
    #[test]
    fn calldata_keeps_its_leading_zeros() {
        assert_ne!(
            k(
                Method::EthCall,
                serde_json::json!([{"to": LOWER, "data": "0x0a"}])
            ),
            k(
                Method::EthCall,
                serde_json::json!([{"to": LOWER, "data": "0xa"}])
            )
        );
    }

    /// A read at the tip and the same read at a height are different questions
    /// with different lifetimes; sharing a key would serve a historical answer
    /// as current.
    #[test]
    fn latest_and_an_explicit_height_do_not_share_a_key() {
        let at_latest = k(
            Method::EthCall,
            serde_json::json!([{"to": LOWER, "data": "0x01"}, "latest"]),
        );
        let at_height = k(
            Method::EthCall,
            serde_json::json!([{"to": LOWER, "data": "0x01"}, "0x1234"]),
        );
        assert_ne!(at_latest, at_height);
    }

    /// Two chains run the same contracts at the same addresses. Without the
    /// chain id in the key, Base would serve Arbitrum's balances.
    #[test]
    fn chains_never_share_a_key() {
        let params: Vec<Value> =
            serde_json::from_value(serde_json::json!([LOWER, "latest"])).unwrap();
        assert_ne!(
            key(1, Method::EthGetBalance, &params),
            key(8453, Method::EthGetBalance, &params)
        );
    }

    /// Two methods with identically-shaped params must not collide.
    #[test]
    fn methods_never_share_a_key() {
        let hash = format!("0x{}", "ab".repeat(32));
        assert_ne!(
            k(Method::EthGetTransactionReceipt, serde_json::json!([hash])),
            k(Method::EthGetTransactionByHash, serde_json::json!([hash]))
        );
    }

    /// The two legal spellings of a single-address filter mean the same query.
    #[test]
    fn the_two_get_logs_address_forms_share_a_key() {
        assert_eq!(
            k(
                Method::EthGetLogs,
                serde_json::json!([{"address": LOWER, "fromBlock": "0x1", "toBlock": "0x2"}])
            ),
            k(
                Method::EthGetLogs,
                serde_json::json!([{"address": [CHECKSUMMED], "fromBlock": "0x1", "toBlock": "0x2"}])
            )
        );
    }

    /// A wildcard slot and a matched slot are different filters. Flattening
    /// topics would return one deposit's log for another's query.
    #[test]
    fn topic_position_and_nesting_are_significant() {
        let a = k(
            Method::EthGetLogs,
            serde_json::json!([{"address": LOWER, "fromBlock": "0x1", "toBlock": "0x2",
                               "topics": ["0xaa", null]}]),
        );
        let b = k(
            Method::EthGetLogs,
            serde_json::json!([{"address": LOWER, "fromBlock": "0x1", "toBlock": "0x2",
                               "topics": [null, "0xaa"]}]),
        );
        let c = k(
            Method::EthGetLogs,
            serde_json::json!([{"address": LOWER, "fromBlock": "0x1", "toBlock": "0x2",
                               "topics": ["0xaa"]}]),
        );
        assert_ne!(a, b, "position matters");
        assert_ne!(a, c, "arity matters");

        // Case within a topic does not.
        let upper = k(
            Method::EthGetLogs,
            serde_json::json!([{"address": LOWER, "fromBlock": "0x1", "toBlock": "0x2",
                               "topics": ["0xAA", null]}]),
        );
        assert_eq!(a, upper);
    }

    /// Fields that do not change an `eth_call` result must not split the key,
    /// or one client's habit of sending `gasPrice` halves the hit ratio for
    /// everyone.
    #[test]
    fn irrelevant_call_fields_do_not_split_the_key() {
        assert_eq!(
            k(
                Method::EthCall,
                serde_json::json!([{"to": LOWER, "data": "0x01"}])
            ),
            k(
                Method::EthCall,
                serde_json::json!([{"to": LOWER, "data": "0x01", "gas": "0x1c9c380",
                                   "gasPrice": "0x1", "nonce": "0x5"}])
            )
        );
    }

    /// `data` and `input` are the same field under two names.
    #[test]
    fn the_two_calldata_field_names_share_a_key() {
        assert_eq!(
            k(
                Method::EthCall,
                serde_json::json!([{"to": LOWER, "data": "0x70a08231"}])
            ),
            k(
                Method::EthCall,
                serde_json::json!([{"to": LOWER, "input": "0x70a08231"}])
            )
        );
    }

    /// A caller distinguishing `from` gets a distinct entry: `balanceOf` does
    /// not care, but a contract reading `msg.sender` would.
    #[test]
    fn a_different_caller_gets_a_different_key() {
        assert_ne!(
            k(
                Method::EthCall,
                serde_json::json!([{"to": LOWER, "data": "0x01"}])
            ),
            k(
                Method::EthCall,
                serde_json::json!([{"to": LOWER, "from": CHECKSUMMED, "data": "0x01"}])
            )
        );
    }

    /// A full block and a header are different answers under the same tag.
    #[test]
    fn the_full_transaction_flag_is_part_of_the_key() {
        assert_ne!(
            k(
                Method::EthGetBlockByNumber,
                serde_json::json!(["0x1", true])
            ),
            k(
                Method::EthGetBlockByNumber,
                serde_json::json!(["0x1", false])
            )
        );
        // An omitted flag is `false`, as at every node.
        assert_eq!(
            k(Method::EthGetBlockByNumber, serde_json::json!(["0x1"])),
            k(
                Method::EthGetBlockByNumber,
                serde_json::json!(["0x1", false])
            )
        );
    }
}
