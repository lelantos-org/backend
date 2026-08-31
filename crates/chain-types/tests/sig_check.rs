use alloy::sol_types::SolEvent;
use chain_types::abi::{
    AssetFeeSet, AssetMoved, AssetRegistered, DepositCanceled, DepositEscrowed, DepositFlushed,
    EmergencyUnwound, HaltedSet, NormalizedFeeSwept, NotePayload, NullifierConsumed,
    PerfFeeAccrued, Rebalanced, RootAdvanced, YieldAssetAdded, YieldParamsSet,
};

/// Topic0 of every event this crate decodes, taken from the canonical Foundry
/// ABI (`contracts/packages/abi/json/*.json`) rather than from the `sol!`
/// declarations these assertions check.
///
/// The `sol!` block is a hand-written copy of the contract's events and compiles
/// whatever it says. A field that drifts from the contract changes the signature
/// hash, so the indexer stops matching the log and deposits vanish from the
/// pipeline without an error. Pinning the hashes against the contract's own ABI
/// turns that into a test failure. Regenerate by hashing the signature built from
/// the ABI JSON.
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
        "5123881c5897fb743e8137f8b19854b64eb895f9d2311b9995397c3b8eff0c4a",
    ),
    (
        "AssetFeeSet",
        "a8dda59a05d622fd0130a6952f3e1fedeb17964708e5a6b3d186ce77cd69961c",
    ),
    (
        "YieldAssetAdded",
        "5ae984b9affbaf15a0cd4631d9fc011bb75bdb1a8e2da3cf13907a6ba18ba418",
    ),
    (
        "YieldParamsSet",
        "4ca5866c0899baa0a170281d0370a9bf9ffe83c56c4c37c2c59a0a9f07b3afb8",
    ),
    (
        "PerfFeeAccrued",
        "e859634206ca91269b2ded4ebcf8ab2af2974ef3a25b29e6e31104ff99f31886",
    ),
    (
        "NormalizedFeeSwept",
        "d4c0820935c555940002a6883fab7ba3769f411649844c4ec62b1c7c93e17aa5",
    ),
    (
        "Rebalanced",
        "09dbf3bd3f8a16776d0cd926612836410135cd5ef32b6cae9c9f182b923412ae",
    ),
    (
        "HaltedSet",
        "d05d75ff4dcbb98f9bc9448ffbb36e9fa2684426abf92840d4587afd8b829f75",
    ),
    (
        "EmergencyUnwound",
        "4385959e0b5d182a2d8fb896697c2034d6e959bec6b97907a6846036cf4f10cf",
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
        ("AssetFeeSet", AssetFeeSet::SIGNATURE_HASH),
        ("YieldAssetAdded", YieldAssetAdded::SIGNATURE_HASH),
        ("YieldParamsSet", YieldParamsSet::SIGNATURE_HASH),
        ("PerfFeeAccrued", PerfFeeAccrued::SIGNATURE_HASH),
        ("NormalizedFeeSwept", NormalizedFeeSwept::SIGNATURE_HASH),
        ("Rebalanced", Rebalanced::SIGNATURE_HASH),
        ("HaltedSet", HaltedSet::SIGNATURE_HASH),
        ("EmergencyUnwound", EmergencyUnwound::SIGNATURE_HASH),
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
