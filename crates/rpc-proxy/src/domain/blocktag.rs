//! Block tags, and the hex-quantity parsing they rest on.
//!
//! Split out because three separate decisions read a block tag and must read it
//! identically: whether a request is allowed at all (`earliest` is refused),
//! which cache class it lands in (a finalized height is immutable, `latest` is
//! not), and how the cache key is canonicalised (`0x01a` and `0x1a` are the same
//! block). A tag parsed one way here and another way there would show up as a
//! cache that quietly stops hitting.

use serde_json::Value;

/// Which block a read is against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockTag {
    /// An explicit height. The only variant that can be finalized, and so the
    /// only one whose answer is cacheable for longer than a couple of seconds.
    Number(u64),
    Latest,
    Pending,
    Safe,
    Finalized,
    /// Present in the request but not a tag this proxy understands.
    ///
    /// Distinct from "absent": an unrecognised tag must be refused rather than
    /// defaulted to `latest`, since defaulting would answer a question the
    /// caller did not ask.
    Unknown,
}

impl BlockTag {
    /// Parse a tag from a JSON param.
    ///
    /// An absent or `null` param is [`BlockTag::Latest`], which is what every
    /// EVM node does with a missing block argument. Materialising the default
    /// here rather than leaving it implicit is what stops `eth_call` with and
    /// without a trailing `"latest"` producing two cache keys for one question.
    pub fn parse(v: Option<&Value>) -> Self {
        match v {
            None | Some(Value::Null) => BlockTag::Latest,
            Some(Value::String(s)) => match s.as_str() {
                "latest" => BlockTag::Latest,
                "pending" => BlockTag::Pending,
                "safe" => BlockTag::Safe,
                "finalized" => BlockTag::Finalized,
                // Not mapped to `Number(0)`. Genesis is a valid height, but
                // `earliest` denotes an archive query, which the allowlist
                // refuses by name.
                "earliest" => BlockTag::Unknown,
                s => parse_quantity(s).map_or(BlockTag::Unknown, BlockTag::Number),
            },
            // Some clients send a bare integer rather than a hex quantity.
            Some(Value::Number(n)) => n.as_u64().map_or(BlockTag::Unknown, BlockTag::Number),
            _ => BlockTag::Unknown,
        }
    }

    /// The height, for a tag that names one.
    pub fn number(self) -> Option<u64> {
        match self {
            BlockTag::Number(n) => Some(n),
            _ => None,
        }
    }

    /// The canonical string form, for cache keys.
    ///
    /// A height always renders minimally (`0x1a`, never `0x01a`), so two clients
    /// spelling the same block differently share one entry.
    pub fn canonical(self) -> String {
        match self {
            BlockTag::Number(n) => format!("0x{n:x}"),
            BlockTag::Latest => "latest".into(),
            BlockTag::Pending => "pending".into(),
            BlockTag::Safe => "safe".into(),
            BlockTag::Finalized => "finalized".into(),
            BlockTag::Unknown => "unknown".into(),
        }
    }
}

/// Parse a `0x`-prefixed hex quantity.
///
/// Strict about the prefix and about emptiness, lenient about leading zeros:
/// the JSON-RPC spec forbids them but clients emit them anyway, and refusing
/// would break a caller over a formatting detail that changes no meaning.
pub fn parse_quantity(s: &str) -> Option<u64> {
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    if hex.is_empty() {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

/// Whether a 32-byte hash is well-formed: `0x` and exactly 64 hex digits.
///
/// Length-checked rather than merely hex-checked so a truncated hash is refused
/// here instead of becoming a cache key that can never be hit again.
pub fn is_hash32(v: &Value) -> bool {
    let Some(s) = v.as_str() else { return false };
    let Some(hex) = s.strip_prefix("0x") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether a value is a well-formed 20-byte address.
pub fn is_address(v: &Value) -> bool {
    let Some(s) = v.as_str() else { return false };
    let Some(hex) = s.strip_prefix("0x") else {
        return false;
    };
    hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tag(v: serde_json::Value) -> BlockTag {
        BlockTag::parse(Some(&v))
    }

    /// The default that keeps `eth_call` with and without a trailing tag on one
    /// cache key.
    #[test]
    fn an_absent_tag_is_latest() {
        assert_eq!(BlockTag::parse(None), BlockTag::Latest);
        assert_eq!(BlockTag::parse(Some(&Value::Null)), BlockTag::Latest);
    }

    #[test]
    fn named_tags_parse() {
        assert_eq!(tag(json!("latest")), BlockTag::Latest);
        assert_eq!(tag(json!("pending")), BlockTag::Pending);
        assert_eq!(tag(json!("safe")), BlockTag::Safe);
        assert_eq!(tag(json!("finalized")), BlockTag::Finalized);
    }

    /// `earliest` is an archive query on any pruning node, so it is refused by
    /// name rather than quietly read as block zero.
    #[test]
    fn earliest_is_unknown_not_block_zero() {
        assert_eq!(tag(json!("earliest")), BlockTag::Unknown);
    }

    /// The canonicalisation the cache depends on: two spellings of one height
    /// must produce one key, or the hit ratio silently collapses.
    #[test]
    fn leading_zeros_do_not_change_a_height() {
        assert_eq!(tag(json!("0x01a")), BlockTag::Number(26));
        assert_eq!(tag(json!("0x1a")), BlockTag::Number(26));
        assert_eq!(
            tag(json!("0x01a")).canonical(),
            tag(json!("0x1a")).canonical()
        );
    }

    /// An unrecognised tag must not default to `latest`: answering at the tip a
    /// question asked about some other block is a wrong answer, not a lenient
    /// one.
    #[test]
    fn a_malformed_tag_is_unknown_rather_than_latest() {
        assert_eq!(tag(json!("0x")), BlockTag::Unknown);
        assert_eq!(tag(json!("banana")), BlockTag::Unknown);
        assert_eq!(tag(json!("0xnothex")), BlockTag::Unknown);
        assert_eq!(tag(json!(true)), BlockTag::Unknown);
    }

    #[test]
    fn a_bare_integer_height_is_accepted() {
        assert_eq!(tag(json!(26)), BlockTag::Number(26));
    }

    /// A truncated hash would otherwise become a cache key nothing can hit and
    /// an upstream call that always fails.
    #[test]
    fn hash_and_address_shapes_are_length_checked() {
        let h = format!("0x{}", "ab".repeat(32));
        assert!(is_hash32(&json!(h)));
        assert!(!is_hash32(&json!("0xabcd")));
        assert!(!is_hash32(&json!(format!("0x{}", "ab".repeat(33)))));
        assert!(!is_hash32(&json!("0xZZ")));

        let a = format!("0x{}", "cd".repeat(20));
        assert!(is_address(&json!(a)));
        assert!(!is_address(&json!("0xabcd")));
        // A hash is not an address, even though both are hex.
        assert!(!is_address(&json!(h)));
    }
}
