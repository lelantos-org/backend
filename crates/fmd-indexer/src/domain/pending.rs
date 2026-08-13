use crate::domain::convert::u256_to_bigdecimal;
use crate::domain::error::FmdIndexerError;
use crate::repositories::notes::NewNote;
use crate::repositories::raw_events::RawEventRow;
use crate::repositories::spent_nullifiers::NewSpentNullifier;
use alloy::primitives::U256;
use chain_types::decode::{self, DecodedEvent};
use shared::entities::EventKind;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use tracing::warn;

/// Per-tx accumulator linking a single `RootAdvanced` to its trailing
/// `NoteCreated` events. Contract emits `RootAdvanced` first (lower
/// log_index), then `NoteCreated` × `inserted`. Cursor only advances past a
/// tx once both halves are observed in the same batch — partial txs at the
/// batch boundary are deferred to the next tick.
///
/// New escrow flow: `flushBatch` emits `IntentFlushed × n` (lower
/// log_indices) then `RootAdvanced`. Each IntentFlushed contributes 2
/// notes whose cm/aux is sourced from a previously-emitted IntentEscrowed
/// event (looked up via `escrowed_aux`).
pub struct PendingTx {
    pub start_index: u64,
    pub inserted: u64,
    /// `true` once a `RootAdvanced` for this tx has been observed in the
    /// batch. NullifierConsumed events fire first (before RootAdvanced) so
    /// a tx is only "complete" once root is set AND notes.len() == inserted.
    pub root_seen: bool,
    pub notes: Vec<NewNote>,
    pub spent_nfs: Vec<NewSpentNullifier>,
    pub last_id: i64,
}

pub struct CommitPlan {
    pub notes: Vec<NewNote>,
    pub spent_nfs: Vec<NewSpentNullifier>,
    pub last_event_id: i64,
    pub last_block_number: i64,
}

/// Per-output FMD payload + cm, decoded from an `IntentEscrowed` event.
/// Pre-resolved by `consume.rs` and passed in so `plan_commit` stays sync.
#[derive(Clone)]
pub struct EscrowedSlot {
    pub cm: Vec<u8>,
    pub clue_rx: U256,
    pub clue_ry: U256,
    pub eph_pub_x: U256,
    pub eph_pub_y: U256,
    pub ciphertext: Vec<u8>,
    pub cv_dep_x: U256,
    pub cv_dep_y: U256,
}

/// Map from intent_id (decimal string) → (slot0, slot1).
pub type EscrowedMap = HashMap<String, [EscrowedSlot; 2]>;

/// Group raw events by tx_hash, decode, and produce a commit plan up to
/// the first incomplete tx. Returns `None` when nothing is fully complete.
///
/// `escrowed` holds pre-resolved IntentEscrowed payloads keyed by intent
/// id. Any IntentFlushed referencing an intent missing from this map is
/// treated as "data not yet ingested" → defer the entire tx (consumer
/// will retry on next tick once the IntentEscrowed event is available).
pub fn plan_commit(
    rows: &[RawEventRow],
    chain_id: i64,
    after: i64,
    escrowed: &EscrowedMap,
) -> Result<Option<CommitPlan>, FmdIndexerError> {
    let mut by_tx: HashMap<Vec<u8>, PendingTx> = HashMap::new();
    let mut tx_order: Vec<Vec<u8>> = Vec::new();
    let mut tx_unresolved: HashMap<Vec<u8>, bool> = HashMap::new();

    for r in rows {
        let kind = match EventKind::from_i16(r.event_kind) {
            Some(k) => k,
            None => continue,
        };
        let decoded = match decode::decode(kind, &r.topics, &r.data) {
            Ok(d) => d,
            Err(e) => {
                warn!("decode error: {}", e);
                continue;
            }
        };
        let p = match by_tx.entry(r.tx_hash.clone()) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => {
                tx_order.push(v.key().clone());
                v.insert(PendingTx {
                    start_index: 0,
                    inserted: 0,
                    root_seen: false,
                    notes: Vec::new(),
                    spent_nfs: Vec::new(),
                    last_id: r.id,
                })
            }
        };

        for ev in decoded {
            match ev {
                DecodedEvent::RootAdvanced {
                    start_index,
                    inserted,
                    ..
                } => {
                    p.start_index = start_index;
                    p.inserted = inserted;
                    p.root_seen = true;
                    p.notes.reserve(inserted as usize);
                    p.last_id = r.id;
                    // Assign leaf indices to pending notes now that start_index is known.
                    for (i, n) in p.notes.iter_mut().enumerate() {
                        n.leaf_index = (start_index + i as u64) as i64;
                    }
                }
                DecodedEvent::NoteCreated {
                    cm,
                    clue_rx,
                    clue_ry,
                    eph_pub_x,
                    eph_pub_y,
                    ciphertext,
                    cv_dep_x,
                    cv_dep_y,
                } => {
                    // Block coordinates only: a tx hash in the log is a
                    // direct pointer from an operator's log stream into the
                    // note -> intent -> payer/recipient join.
                    if !p.root_seen {
                        warn!(
                            block_number = r.block_number,
                            log_index = r.log_index,
                            "NoteCreated before RootAdvanced; skipping"
                        );
                        continue;
                    }
                    if (p.notes.len() as u64) >= p.inserted {
                        warn!(
                            block_number = r.block_number,
                            log_index = r.log_index,
                            "extra NoteCreated beyond inserted count"
                        );
                        continue;
                    }
                    if ciphertext.len() < 2 {
                        warn!("ciphertext too short for clueBits prefix");
                        continue;
                    }
                    let leaf_index = (p.start_index + p.notes.len() as u64) as i64;
                    p.notes.push(NewNote {
                        chain_id,
                        block_number: r.block_number,
                        tx_hash: r.tx_hash.clone(),
                        log_index: r.log_index,
                        cm: cm.0.to_vec(),
                        clue_rx: u256_to_bigdecimal(clue_rx),
                        clue_ry: u256_to_bigdecimal(clue_ry),
                        eph_pub_x: u256_to_bigdecimal(eph_pub_x),
                        eph_pub_y: u256_to_bigdecimal(eph_pub_y),
                        ciphertext,
                        leaf_index,
                        cv_dep_x: u256_to_bigdecimal(cv_dep_x),
                        cv_dep_y: u256_to_bigdecimal(cv_dep_y),
                    });
                    p.last_id = r.id;
                }
                DecodedEvent::NullifierConsumed { nf } => {
                    p.spent_nfs.push(NewSpentNullifier {
                        chain_id,
                        block_number: r.block_number,
                        log_index: r.log_index,
                        nf: nf.0.to_vec(),
                        tx_hash: r.tx_hash.clone(),
                        block_ts: r.block_ts,
                    });
                    p.last_id = r.id;
                }
                DecodedEvent::IntentFlushed { id, .. } => {
                    let key = id.to_string();
                    let Some(slots) = escrowed.get(&key) else {
                        // IntentEscrowed not yet ingested → defer the whole tx.
                        tx_unresolved.insert(r.tx_hash.clone(), true);
                        continue;
                    };
                    for slot in slots {
                        if slot.ciphertext.len() < 2 {
                            warn!("escrowed ciphertext too short for clueBits prefix");
                            continue;
                        }
                        // leaf_index is provisional (0); RootAdvanced overwrites it.
                        p.notes.push(NewNote {
                            chain_id,
                            block_number: r.block_number,
                            tx_hash: r.tx_hash.clone(),
                            log_index: r.log_index,
                            cm: slot.cm.clone(),
                            clue_rx: u256_to_bigdecimal(slot.clue_rx),
                            clue_ry: u256_to_bigdecimal(slot.clue_ry),
                            eph_pub_x: u256_to_bigdecimal(slot.eph_pub_x),
                            eph_pub_y: u256_to_bigdecimal(slot.eph_pub_y),
                            ciphertext: slot.ciphertext.clone(),
                            leaf_index: 0,
                            cv_dep_x: u256_to_bigdecimal(slot.cv_dep_x),
                            cv_dep_y: u256_to_bigdecimal(slot.cv_dep_y),
                        });
                    }
                    p.last_id = r.id;
                }
                _ => {}
            }
        }
    }

    let mut commit_notes: Vec<NewNote> = Vec::new();
    let mut commit_spent_nfs: Vec<NewSpentNullifier> = Vec::new();
    let mut last_committed_id = after;
    let mut last_committed_block = 0i64;
    for tx_hash in &tx_order {
        if tx_unresolved.contains_key(tx_hash) {
            break;
        }
        {
            let p = by_tx.get(tx_hash).expect("present");
            if !p.root_seen || (p.notes.len() as u64) != p.inserted {
                break;
            }
            last_committed_id = p.last_id;
            if let Some(n) = p.notes.last() {
                last_committed_block = n.block_number;
            }
        }
        let mut p = by_tx.remove(tx_hash).expect("present");
        commit_notes.append(&mut p.notes);
        commit_spent_nfs.append(&mut p.spent_nfs);
    }

    if last_committed_id == after {
        return Ok(None);
    }

    Ok(Some(CommitPlan {
        notes: commit_notes,
        spent_nfs: commit_spent_nfs,
        last_event_id: last_committed_id,
        last_block_number: last_committed_block,
    }))
}
