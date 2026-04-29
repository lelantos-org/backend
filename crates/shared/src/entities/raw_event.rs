use crate::chain::ChainId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
pub enum EventKind {
    NoteCreated = 1,
    AssetRegistered = 2,
    RootAdvanced = 3,
    AssetMoved = 4,
    NullifierConsumed = 5,
    IntentEscrowed = 6,
    IntentFlushed = 7,
    IntentCanceled = 8,
}

impl EventKind {
    pub fn from_i16(v: i16) -> Option<Self> {
        match v {
            1 => Some(Self::NoteCreated),
            2 => Some(Self::AssetRegistered),
            3 => Some(Self::RootAdvanced),
            4 => Some(Self::AssetMoved),
            5 => Some(Self::NullifierConsumed),
            6 => Some(Self::IntentEscrowed),
            7 => Some(Self::IntentFlushed),
            8 => Some(Self::IntentCanceled),
            _ => None,
        }
    }

    pub fn as_i16(self) -> i16 {
        self as i16
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEvent {
    pub id: i64,
    pub chain_id: ChainId,
    pub block_number: i64,
    pub block_hash: Vec<u8>,
    pub block_ts: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub event_kind: EventKind,
    pub topics: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}
