use crate::abi::{
    AssetMoved, AssetRegistered, IntentCanceled, IntentEscrowed, IntentFlushed, NotePayload,
    NullifierConsumed, RootAdvanced,
};
use alloy::primitives::{Address, B256, LogData, U256};
use alloy::sol_types::SolEvent;
use shared::entities::EventKind;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("unknown event kind: {0}")]
    UnknownKind(i16),
    #[error("alloy decode error: {0}")]
    Alloy(String),
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum DecodedEvent {
    NoteCreated {
        cm: B256,
        clue_rx: U256,
        clue_ry: U256,
        eph_pub_x: U256,
        eph_pub_y: U256,
        ciphertext: Vec<u8>,
        cv_dep_x: U256,
        cv_dep_y: U256,
    },
    AssetRegistered {
        asset_id: u64,
        token: Address,
        scale: U256,
    },
    RootAdvanced {
        start_index: u64,
        inserted: u64,
        old_root: B256,
        new_root: B256,
    },
    AssetMoved {
        asset_id: u64,
        token: Address,
        in_amount: U256,
        out_amount: U256,
    },
    NullifierConsumed {
        nf: B256,
    },
    IntentEscrowed {
        id: U256,
        payer: Address,
        recipient: Address,
        public_asset_id: u64,
        public_in: u64,
        fee_bps_at_submit: u16,
        cm0: B256,
        cm1: B256,
        cv_dep0_x: U256,
        cv_dep0_y: U256,
        cv_dep1_x: U256,
        cv_dep1_y: U256,
        rcv_total: U256,
        clue_rx0: U256,
        clue_ry0: U256,
        eph_pub_x0: U256,
        eph_pub_y0: U256,
        ciphertext0: Vec<u8>,
        clue_rx1: U256,
        clue_ry1: U256,
        eph_pub_x1: U256,
        eph_pub_y1: U256,
        ciphertext1: Vec<u8>,
    },
    IntentFlushed {
        id: U256,
        cm0: B256,
        cm1: B256,
    },
    IntentCanceled {
        id: U256,
        payer: Address,
        refunded: U256,
    },
}

/// Decode one source log into one or more `DecodedEvent`s.
///
/// All event kinds yield exactly one entry except `EventKind::NoteCreated`,
/// which fans the packed `NotePayload` log into two
/// `DecodedEvent::NoteCreated` entries — one per output commitment. The slim
/// `NotesCreated(cm0, cm1)` event is informational only and is not indexed
/// by the backend.
pub fn decode(
    event_kind: EventKind,
    topics: &[Vec<u8>],
    data: &[u8],
) -> Result<Vec<DecodedEvent>, DecodeError> {
    let topics: Vec<B256> = topics
        .iter()
        .map(|t| B256::from_slice(t.as_slice()))
        .collect();
    let log = LogData::new_unchecked(topics, data.to_vec().into());
    match event_kind {
        EventKind::NoteCreated => {
            let ev = NotePayload::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![
                DecodedEvent::NoteCreated {
                    cm: ev.cm0,
                    clue_rx: ev.clueRx0,
                    clue_ry: ev.clueRy0,
                    eph_pub_x: ev.ephPubX0,
                    eph_pub_y: ev.ephPubY0,
                    ciphertext: ev.ciphertext0.to_vec(),
                    cv_dep_x: ev.cvDep0X,
                    cv_dep_y: ev.cvDep0Y,
                },
                DecodedEvent::NoteCreated {
                    cm: ev.cm1,
                    clue_rx: ev.clueRx1,
                    clue_ry: ev.clueRy1,
                    eph_pub_x: ev.ephPubX1,
                    eph_pub_y: ev.ephPubY1,
                    ciphertext: ev.ciphertext1.to_vec(),
                    cv_dep_x: ev.cvDep1X,
                    cv_dep_y: ev.cvDep1Y,
                },
            ])
        }
        EventKind::AssetRegistered => {
            let ev = AssetRegistered::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::AssetRegistered {
                asset_id: ev.assetId,
                token: ev.token,
                scale: ev.scale,
            }])
        }
        EventKind::RootAdvanced => {
            let ev = RootAdvanced::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::RootAdvanced {
                start_index: ev.startIndex,
                inserted: ev.inserted,
                old_root: ev.oldRoot,
                new_root: ev.newRoot,
            }])
        }
        EventKind::AssetMoved => {
            let ev = AssetMoved::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::AssetMoved {
                asset_id: ev.assetId,
                token: ev.token,
                in_amount: ev.inAmount,
                out_amount: ev.outAmount,
            }])
        }
        EventKind::NullifierConsumed => {
            let ev = NullifierConsumed::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::NullifierConsumed { nf: ev.nf }])
        }
        EventKind::IntentEscrowed => {
            let ev = IntentEscrowed::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::IntentEscrowed {
                id: ev.id,
                payer: ev.payer,
                recipient: ev.recipient,
                public_asset_id: ev.publicAssetId,
                public_in: ev.publicIn,
                fee_bps_at_submit: ev.feeBpsAtSubmit,
                cm0: ev.cm0,
                cm1: ev.cm1,
                cv_dep0_x: ev.cvDep0X,
                cv_dep0_y: ev.cvDep0Y,
                cv_dep1_x: ev.cvDep1X,
                cv_dep1_y: ev.cvDep1Y,
                rcv_total: ev.rcvTotal,
                clue_rx0: ev.clueRx0,
                clue_ry0: ev.clueRy0,
                eph_pub_x0: ev.ephPubX0,
                eph_pub_y0: ev.ephPubY0,
                ciphertext0: ev.ciphertext0.to_vec(),
                clue_rx1: ev.clueRx1,
                clue_ry1: ev.clueRy1,
                eph_pub_x1: ev.ephPubX1,
                eph_pub_y1: ev.ephPubY1,
                ciphertext1: ev.ciphertext1.to_vec(),
            }])
        }
        EventKind::IntentFlushed => {
            let ev = IntentFlushed::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::IntentFlushed {
                id: ev.id,
                cm0: ev.cm0,
                cm1: ev.cm1,
            }])
        }
        EventKind::IntentCanceled => {
            let ev = IntentCanceled::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::IntentCanceled {
                id: ev.id,
                payer: ev.payer,
                refunded: ev.refunded,
            }])
        }
    }
}

pub fn event_kind_from_topic0(topic0: &B256) -> Option<EventKind> {
    if topic0 == &NotePayload::SIGNATURE_HASH {
        Some(EventKind::NoteCreated)
    } else if topic0 == &AssetRegistered::SIGNATURE_HASH {
        Some(EventKind::AssetRegistered)
    } else if topic0 == &RootAdvanced::SIGNATURE_HASH {
        Some(EventKind::RootAdvanced)
    } else if topic0 == &AssetMoved::SIGNATURE_HASH {
        Some(EventKind::AssetMoved)
    } else if topic0 == &NullifierConsumed::SIGNATURE_HASH {
        Some(EventKind::NullifierConsumed)
    } else if topic0 == &IntentEscrowed::SIGNATURE_HASH {
        Some(EventKind::IntentEscrowed)
    } else if topic0 == &IntentFlushed::SIGNATURE_HASH {
        Some(EventKind::IntentFlushed)
    } else if topic0 == &IntentCanceled::SIGNATURE_HASH {
        Some(EventKind::IntentCanceled)
    } else {
        None
    }
}

pub fn known_signatures() -> [B256; 8] {
    [
        NotePayload::SIGNATURE_HASH,
        AssetRegistered::SIGNATURE_HASH,
        RootAdvanced::SIGNATURE_HASH,
        AssetMoved::SIGNATURE_HASH,
        NullifierConsumed::SIGNATURE_HASH,
        IntentEscrowed::SIGNATURE_HASH,
        IntentFlushed::SIGNATURE_HASH,
        IntentCanceled::SIGNATURE_HASH,
    ]
}
