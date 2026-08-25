use alloy::sol_types::SolEvent;
use chain_types::abi::{
    AssetMoved, AssetRegistered, DepositCanceled, DepositEscrowed, DepositFlushed, NotePayload,
    NullifierConsumed, RootAdvanced,
};

/// Topic0 of every event this crate decodes, taken from the canonical Foundry
/// ABI (`contracts/packages/abi/json/*.json`) rather than from the `sol!`
/// declarations these assertions check.
///
/// The `sol!` block is a hand-written copy of the contract's events. It
/// compiles no matter what it says, and a field that drifts from the contract
/// changes the signature hash — so the indexer simply stops matching the log
/// and deposits silently vanish from the pipeline, with no error anywhere.
/// Pinning the hashes against the contract's own ABI turns that into a test
/// failure. Regenerate by hashing the signature built from the ABI JSON.
const EXPECTED: &[(&str, &str)] = &[
    (
        "DepositEscrowed",
        "ccc71a318d782f72ed7aaea4e3bd8cad9f99a45c9f27927ab48b82c3deb06c1c",
    ),
    (
        "DepositFlushed",
        "33440b1d7e5651195195f83aa503d8a1370a7ac7ce52cc70cfb937542351ecc5",
    ),
    (
        "DepositCanceled",
        "42163e0f65cf33474a2278520f1d0ad5d266e9ed49d005b2335ef8f35f781816",
    ),
    (
        "NotePayload",
        "08829d53b88cc31ed8597c58d2cc3202054ab57e9ab21b258aec2ae0974aa8d7",
    ),
    (
        "NullifierConsumed",
        "6159549712b421860b1a73100a45b4216017d27fe478a58c386f8ced10b7e1b7",
    ),
    (
        "RootAdvanced",
        "616c77b191d495f23f0e9878ac4c2eec8291e5d6aecc4a1ea1866dcdf3a4495a",
    ),
    (
        "AssetRegistered",
        "3e23e248de6b3b2f4af21d83ffebc319973fc90994acc3a445dfbcfe57255ce3",
    ),
    (
        "AssetMoved",
        "e518860ada3bf432f9af64f54465c559c978ecbe2a9d6711b16532dea1daa4a6",
    ),
];

#[test]
fn test_signature_hashes_match_the_deployed_contract_abi() {
    let actual = [
        ("DepositEscrowed", DepositEscrowed::SIGNATURE_HASH),
        ("DepositFlushed", DepositFlushed::SIGNATURE_HASH),
        ("DepositCanceled", DepositCanceled::SIGNATURE_HASH),
        ("NotePayload", NotePayload::SIGNATURE_HASH),
        ("NullifierConsumed", NullifierConsumed::SIGNATURE_HASH),
        ("RootAdvanced", RootAdvanced::SIGNATURE_HASH),
        ("AssetRegistered", AssetRegistered::SIGNATURE_HASH),
        ("AssetMoved", AssetMoved::SIGNATURE_HASH),
    ];

    for (name, hash) in actual {
        let want = EXPECTED
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, h)| *h)
            .unwrap_or_else(|| panic!("no pinned topic0 for {name}"));
        assert_eq!(
            hex::encode(hash.0),
            want,
            "{name}: `sol!` declaration disagrees with the contract ABI"
        );
    }
}
