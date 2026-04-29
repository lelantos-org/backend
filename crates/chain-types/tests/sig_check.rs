use alloy::sol_types::SolEvent;
use chain_types::abi::{
    AssetMoved, AssetRegistered, IntentCanceled, IntentEscrowed, IntentFlushed, NotePayload,
    NotesCreated, NullifierConsumed, RootAdvanced,
};

#[test]
fn print_sigs() {
    eprintln!(
        "IntentEscrowed: {}",
        hex::encode(IntentEscrowed::SIGNATURE_HASH.0)
    );
    eprintln!(
        "IntentFlushed: {}",
        hex::encode(IntentFlushed::SIGNATURE_HASH.0)
    );
    eprintln!(
        "IntentCanceled: {}",
        hex::encode(IntentCanceled::SIGNATURE_HASH.0)
    );
    eprintln!(
        "NotesCreated: {}",
        hex::encode(NotesCreated::SIGNATURE_HASH.0)
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
