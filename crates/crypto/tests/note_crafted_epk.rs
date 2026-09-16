//! SDK/backend agreement on crafted `epk` values.
//!
//! `vectors/note-parity.json` covers honest notes, which is what the SDK's
//! encrypt path can produce. It cannot produce an `epk` carrying an 8-torsion
//! term, so it cannot pin the behaviour the cofactor clearing exists for.
//!
//! These cases were emitted from the SDK against the same key: one honest note,
//! six with `epk = T + [esk]B8` for each torsion point that decompresses, and
//! eight pure-torsion `epk`s. Each records what the SDK returned; this asserts
//! the backend returns the same. Two independent implementations agreeing on
//! inputs neither was built from is the property at stake.

use crypto::note::try_decrypt;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    label: String,
    epk: String,
    ct: String,
    /// Hex plaintext the SDK returned, or `null` if it refused.
    sdk: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    ivk_be: String,
    cases: Vec<Case>,
}

fn bytes32(hex_str: &str) -> [u8; 32] {
    hex::decode(hex_str)
        .expect("hex")
        .try_into()
        .expect("32 bytes")
}

#[test]
fn agrees_with_the_sdk_on_crafted_epk() {
    let f: Fixture = serde_json::from_str(include_str!("vectors/note-crafted-epk.json"))
        .expect("note-crafted-epk.json parses");
    let ivk = bytes32(&f.ivk_be);

    let mut decrypted = 0;
    let mut refused = 0;
    for c in &f.cases {
        let got =
            try_decrypt(&ivk, &bytes32(&c.epk), &hex::decode(&c.ct).expect("hex")).map(hex::encode);
        assert_eq!(got, c.sdk, "disagreed with the SDK on `{}`", c.label);
        if got.is_some() {
            decrypted += 1
        } else {
            refused += 1
        }
    }

    // Guards the fixture itself: a file that lost its crafted cases, or one
    // where everything started refusing, would otherwise pass silently.
    assert_eq!(decrypted, 7, "honest note plus six torsion-perturbed");
    assert_eq!(refused, 8, "every pure-torsion epk");
}
