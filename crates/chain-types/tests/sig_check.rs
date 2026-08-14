use alloy::sol_types::SolEvent;
use chain_types::abi::{
    AssetMoved, AssetRegistered, DepositCanceled, DepositEscrowed, DepositFlushed, NotePayload,
    NullifierConsumed, RootAdvanced,
};

#[test]
fn print_sigs() {
    eprintln!(
        "DepositEscrowed: {}",
        hex::encode(DepositEscrowed::SIGNATURE_HASH.0)
    );
    eprintln!(
        "DepositFlushed: {}",
        hex::encode(DepositFlushed::SIGNATURE_HASH.0)
    );
    eprintln!(
        "DepositCanceled: {}",
        hex::encode(DepositCanceled::SIGNATURE_HASH.0)
    );
    eprintln!(
        "NotePayload: {}",
        hex::encode(NotePayload::SIGNATURE_HASH.0)
    );
    eprintln!(
        "AssetRegistered: {}",
        hex::encode(AssetRegistered::SIGNATURE_HASH.0)
    );
    eprintln!("AssetMoved: {}", hex::encode(AssetMoved::SIGNATURE_HASH.0));
    eprintln!(
        "RootAdvanced: {}",
        hex::encode(RootAdvanced::SIGNATURE_HASH.0)
    );
    eprintln!(
        "NullifierConsumed: {}",
        hex::encode(NullifierConsumed::SIGNATURE_HASH.0)
    );
}
