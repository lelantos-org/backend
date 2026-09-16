//! Log decode: a stored `raw_events` row back into the event it records.

mod event;
mod signatures;

pub use event::{DecodedEvent, DepositFeeNote};
pub use signatures::{event_kind_from_topic0, known_signatures};

use crate::abi::{
    AssetFeeSet, AssetMoved, AssetRegistered, DepositCanceled, DepositEscrowed, DepositFlushed,
    EmergencyUnwound, HaltedSet, NormalizedFeeSwept, NotePayload, NullifierConsumed,
    PerfFeeAccrued, Rebalanced, RootAdvanced, YieldAssetAdded, YieldParamsSet,
};
use alloy::primitives::{B256, LogData};
use alloy::sol_types::SolEvent;
use shared::entities::EventKind;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("alloy decode error: {0}")]
    Alloy(String),
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
    let event = match event_kind {
        EventKind::NoteCreated => {
            let ev: NotePayload = decode_log(&log)?;
            DecodedEvent::NoteCreated {
                cm: ev.cm,
                clue_rx: ev.clueRx,
                clue_ry: ev.clueRy,
                eph_pub_x: ev.ephPubX,
                eph_pub_y: ev.ephPubY,
                ciphertext: ev.ciphertext.to_vec(),
                cv_dep_x: ev.cvDepX,
                cv_dep_y: ev.cvDepY,
            }
        }
        EventKind::AssetRegistered => {
            let ev: AssetRegistered = decode_log(&log)?;
            DecodedEvent::AssetRegistered {
                asset_id: ev.assetId,
                token: ev.token,
                scale: ev.scale,
            }
        }
        EventKind::AssetFeeSet => {
            let ev: AssetFeeSet = decode_log(&log)?;
            DecodedEvent::AssetFeeSet {
                asset_id: ev.assetId,
                deposit_bps: ev.depositBps,
                withdraw_bps: ev.withdrawBps,
            }
        }
        EventKind::RootAdvanced => {
            let ev: RootAdvanced = decode_log(&log)?;
            DecodedEvent::RootAdvanced {
                start_index: ev.startIndex,
                inserted: ev.inserted,
                old_root: ev.oldRoot,
                new_root: ev.newRoot,
            }
        }
        EventKind::AssetMoved => {
            let ev: AssetMoved = decode_log(&log)?;
            DecodedEvent::AssetMoved {
                asset_id: ev.assetId,
                token: ev.token,
                in_amount: ev.inAmount,
                out_amount: ev.outAmount,
                public_in: ev.publicIn,
                public_out: ev.publicOut,
            }
        }
        EventKind::NullifierConsumed => {
            let ev: NullifierConsumed = decode_log(&log)?;
            DecodedEvent::NullifierConsumed { nf: ev.nf }
        }
        EventKind::DepositEscrowed => {
            let ev: DepositEscrowed = decode_log(&log)?;
            DecodedEvent::DepositEscrowed {
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
                    fee_asset_id: ev.feeAssetId,
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
            }
        }
        EventKind::DepositFlushed => {
            let ev: DepositFlushed = decode_log(&log)?;
            DecodedEvent::DepositFlushed {
                id: ev.id,
                cm: ev.cm,
            }
        }
        EventKind::DepositCanceled => {
            let ev: DepositCanceled = decode_log(&log)?;
            DecodedEvent::DepositCanceled {
                id: ev.id,
                payer: ev.payer,
                refunded: ev.refunded,
                fee_asset_id: ev.feeAssetId,
                fee_refunded: ev.feeRefunded,
            }
        }
        EventKind::YieldAssetAdded => {
            let ev: YieldAssetAdded = decode_log(&log)?;
            DecodedEvent::YieldAssetAdded {
                asset_id: ev.assetId,
                venue: ev.venue,
                buffer_bps: ev.bufferBps,
                perf_bps: ev.perfBps,
            }
        }
        EventKind::YieldParamsSet => {
            let ev: YieldParamsSet = decode_log(&log)?;
            DecodedEvent::YieldParamsSet {
                asset_id: ev.assetId,
                buffer_bps: ev.bufferBps,
                perf_bps: ev.perfBps,
            }
        }
        EventKind::PerfFeeAccrued => {
            let ev: PerfFeeAccrued = decode_log(&log)?;
            DecodedEvent::PerfFeeAccrued {
                asset_id: ev.assetId,
                units_minted: ev.unitsMinted,
                new_last_idx: ev.newLastIdx,
            }
        }
        EventKind::NormalizedFeeSwept => {
            let ev: NormalizedFeeSwept = decode_log(&log)?;
            DecodedEvent::NormalizedFeeSwept {
                asset_id: ev.assetId,
                units: ev.units,
                amount: ev.amount,
            }
        }
        EventKind::Rebalanced => {
            let ev: Rebalanced = decode_log(&log)?;
            DecodedEvent::Rebalanced {
                asset_id: ev.assetId,
                idle_after: ev.idleAfter,
            }
        }
        EventKind::HaltedSet => {
            let ev: HaltedSet = decode_log(&log)?;
            DecodedEvent::HaltedSet {
                asset_id: ev.assetId,
                halted: ev.halted,
            }
        }
        EventKind::EmergencyUnwound => {
            let ev: EmergencyUnwound = decode_log(&log)?;
            DecodedEvent::EmergencyUnwound {
                asset_id: ev.assetId,
                recovered: ev.recovered,
            }
        }
    };
    Ok(vec![event])
}

/// One log as the event type `E`.
fn decode_log<E: SolEvent>(log: &LogData) -> Result<E, DecodeError> {
    E::decode_log_data(log, true).map_err(|e| DecodeError::Alloy(e.to_string()))
}
