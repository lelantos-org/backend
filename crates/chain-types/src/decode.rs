use crate::abi::{
    AssetFeeSet, AssetMoved, AssetRegistered, DepositCanceled, DepositEscrowed, DepositFlushed,
    NotePayload, NullifierConsumed, RootAdvanced,
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

/// The fee leaf of a `DepositEscrowed`: a note addressed to the party the
/// payer chose to pay for the flush.
///
/// Grouped into one struct because every field is either part of the escrow
/// digest preimage or required to spend the note; a partial set yields a
/// deposit that cannot be flushed.
#[derive(Debug, Clone)]
pub struct DepositFeeNote {
    pub fee_in: u64,
    pub cm: B256,
    pub cv_dep_x: U256,
    pub cv_dep_y: U256,
    pub rcv: U256,
    pub clue_rx: U256,
    pub clue_ry: U256,
    pub eph_pub_x: U256,
    pub eph_pub_y: U256,
    pub ciphertext: Vec<u8>,
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
    /// Per-leg fee rates for one asset. Emitted at registration and on every
    /// change; there is no pool-wide rate to fall back to, so an asset with no
    /// observed `AssetFeeSet` has unknown rates rather than default ones.
    AssetFeeSet {
        asset_id: u64,
        deposit_bps: u16,
        withdraw_bps: u16,
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
        /// ERC-20 base units moved.
        in_amount: U256,
        out_amount: U256,
        /// The same movement in circuit units, as published by the SNARK.
        public_in: u64,
        public_out: u64,
    },
    NullifierConsumed {
        nf: B256,
    },
    DepositEscrowed {
        id: U256,
        payer: Address,
        recipient: Address,
        public_asset_id: u64,
        public_in: u64,
        fee_bps_at_submit: u16,
        cm: B256,
        cv_dep_x: U256,
        cv_dep_y: U256,
        rcv: U256,
        clue_rx: U256,
        clue_ry: U256,
        eph_pub_x: U256,
        eph_pub_y: U256,
        ciphertext: Vec<u8>,
        /// The relayer's fee note — the second leaf every deposit mints.
        /// Carried alongside the depositor's note so consumers see one row per
        /// deposit and the pair cannot be observed half-applied.
        fee: DepositFeeNote,
    },
    DepositFlushed {
        id: U256,
        cm: B256,
    },
    DepositCanceled {
        id: U256,
        payer: Address,
        refunded: U256,
    },
}

/// Decode one source log into a `DecodedEvent`.
///
/// Every kind yields exactly one entry: `NotePayload` is emitted once per
/// output leaf, so no fan-out is required.
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
            Ok(vec![DecodedEvent::NoteCreated {
                cm: ev.cm,
                clue_rx: ev.clueRx,
                clue_ry: ev.clueRy,
                eph_pub_x: ev.ephPubX,
                eph_pub_y: ev.ephPubY,
                ciphertext: ev.ciphertext.to_vec(),
                cv_dep_x: ev.cvDepX,
                cv_dep_y: ev.cvDepY,
            }])
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
        EventKind::AssetFeeSet => {
            let ev = AssetFeeSet::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::AssetFeeSet {
                asset_id: ev.assetId,
                deposit_bps: ev.depositBps,
                withdraw_bps: ev.withdrawBps,
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
                public_in: ev.publicIn,
                public_out: ev.publicOut,
            }])
        }
        EventKind::NullifierConsumed => {
            let ev = NullifierConsumed::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::NullifierConsumed { nf: ev.nf }])
        }
        EventKind::DepositEscrowed => {
            let ev = DepositEscrowed::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::DepositEscrowed {
                id: ev.id,
                payer: ev.payer,
                recipient: ev.recipient,
                public_asset_id: ev.publicAssetId,
                public_in: ev.publicIn,
                fee_bps_at_submit: ev.feeBpsAtSubmit,
                cm: ev.cm,
                cv_dep_x: ev.cvDepX,
                cv_dep_y: ev.cvDepY,
                rcv: ev.rcv,
                clue_rx: ev.clueRx,
                clue_ry: ev.clueRy,
                eph_pub_x: ev.ephPubX,
                eph_pub_y: ev.ephPubY,
                ciphertext: ev.ciphertext.to_vec(),
                fee: DepositFeeNote {
                    fee_in: ev.feeIn,
                    cm: ev.feeCm,
                    cv_dep_x: ev.feeCvDepX,
                    cv_dep_y: ev.feeCvDepY,
                    rcv: ev.feeRcv,
                    clue_rx: ev.feeClueRx,
                    clue_ry: ev.feeClueRy,
                    eph_pub_x: ev.feeEphPubX,
                    eph_pub_y: ev.feeEphPubY,
                    ciphertext: ev.feeCiphertext.to_vec(),
                },
            }])
        }
        EventKind::DepositFlushed => {
            let ev = DepositFlushed::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::DepositFlushed {
                id: ev.id,
                cm: ev.cm,
            }])
        }
        EventKind::DepositCanceled => {
            let ev = DepositCanceled::decode_log_data(&log, true)
                .map_err(|e| DecodeError::Alloy(e.to_string()))?;
            Ok(vec![DecodedEvent::DepositCanceled {
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
    } else if topic0 == &AssetFeeSet::SIGNATURE_HASH {
        Some(EventKind::AssetFeeSet)
    } else if topic0 == &RootAdvanced::SIGNATURE_HASH {
        Some(EventKind::RootAdvanced)
    } else if topic0 == &AssetMoved::SIGNATURE_HASH {
        Some(EventKind::AssetMoved)
    } else if topic0 == &NullifierConsumed::SIGNATURE_HASH {
        Some(EventKind::NullifierConsumed)
    } else if topic0 == &DepositEscrowed::SIGNATURE_HASH {
        Some(EventKind::DepositEscrowed)
    } else if topic0 == &DepositFlushed::SIGNATURE_HASH {
        Some(EventKind::DepositFlushed)
    } else if topic0 == &DepositCanceled::SIGNATURE_HASH {
        Some(EventKind::DepositCanceled)
    } else {
        None
    }
}

pub fn known_signatures() -> [B256; 9] {
    [
        NotePayload::SIGNATURE_HASH,
        AssetRegistered::SIGNATURE_HASH,
        AssetFeeSet::SIGNATURE_HASH,
        RootAdvanced::SIGNATURE_HASH,
        AssetMoved::SIGNATURE_HASH,
        NullifierConsumed::SIGNATURE_HASH,
        DepositEscrowed::SIGNATURE_HASH,
        DepositFlushed::SIGNATURE_HASH,
        DepositCanceled::SIGNATURE_HASH,
    ]
}
