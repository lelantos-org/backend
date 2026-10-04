//! The published `TreeUpdateBatch(11, 8)` vectors, for the tests that pin the
//! batch path against them: leaf, root, digest and `z`.

use crate::domain::batch::PaddedBatch;
use alloy::primitives::{FixedBytes, U256};
use crypto::tree::Field;
use serde_json::Value;
use std::fmt::Debug;
use std::str::FromStr;

/// One published vector, in the relayer's own types.
pub(crate) struct BatchVector {
    pub(crate) name: String,
    pub(crate) old_root: Field,
    pub(crate) new_root: Field,
    pub(crate) start_index: u64,
    /// The frontier the batch advances from.
    pub(crate) frontier_in: Vec<[Field; 3]>,
    pub(crate) batch: PaddedBatch,
    /// The tree leaf of each active slot, `intermediates.leaves[k].leaf`.
    pub(crate) leaves: Vec<Field>,
    pub(crate) digest: U256,
    pub(crate) z: U256,
}

/// Every vector of `tests/vectors/tree_update_batch_8.json`, vendored from
/// `circuits/vectors/tree-update-batch-8.json`.
///
/// A missing file is a hard failure rather than a skip, so a renamed fixture
/// cannot stop these tests from running unnoticed.
pub(crate) fn load() -> Vec<BatchVector> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/vectors/tree_update_batch_8.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("published vectors missing at {}: {e}", path.display()));
    let file: Value = serde_json::from_str(&raw).expect("published vectors parse");
    let cases = list(&file["vectors"]);
    assert!(!cases.is_empty());
    cases.iter().map(vector).collect()
}

fn vector(case: &Value) -> BatchVector {
    let w = &case["witness"];
    BatchVector {
        name: text(&case["name"]).to_string(),
        old_root: field(&w["old_root"]),
        new_root: field(&w["new_root"]),
        start_index: number(&w["start_index"]),
        frontier_in: list(&w["frontier_in"])
            .iter()
            .map(|row| column(row, field))
            .collect(),
        batch: PaddedBatch {
            cms: column(&w["cms"], |cm| FixedBytes::from(field(cm))),
            leaf_asset: column(&w["leaf_asset"], number),
            leaf_public_in: column(&w["leaf_public_in"], number),
            is_deposit: column(&w["is_deposit"], number),
            actual_count: number(&w["actual_count"]),
        },
        leaves: list(&case["intermediates"]["leaves"])
            .iter()
            .map(|l| field(&l["leaf"]))
            .collect(),
        digest: u256(&case["compression"]["digest"]),
        z: u256(&case["compression"]["z"]),
    }
}

fn text(v: &Value) -> &str {
    v.as_str().expect("string")
}

fn list(v: &Value) -> &[Value] {
    v.as_array().expect("array")
}

fn u256(v: &Value) -> U256 {
    U256::from_str_radix(text(v), 10).expect("decimal field element")
}

fn field(v: &Value) -> Field {
    u256(v).to_be_bytes()
}

fn number<T: FromStr<Err: Debug>>(v: &Value) -> T {
    text(v).parse().expect("decimal integer")
}

/// A published array at the width the relayer's type declares; any other width
/// is a vector for another circuit shape.
fn column<T, const N: usize>(v: &Value, item: impl Fn(&Value) -> T) -> [T; N] {
    let items: Vec<T> = list(v).iter().map(item).collect();
    items
        .try_into()
        .unwrap_or_else(|got: Vec<T>| panic!("expected {N} entries, got {}", got.len()))
}
