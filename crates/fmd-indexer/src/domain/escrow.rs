//! The `DepositEscrowed` side lookup a `DepositFlushed` needs.
//!
//! A flush event carries only the deposit id and its `inner`; the FMD payloads
//! of the two leaves it mints were published earlier, by the escrow event that
//! opened the deposit, and so were the public `(asset, value)` each leaf is
//! built from. Everything here is pure decode over rows the caller has already
//! fetched, so the consume service keeps only the lookup.

use crate::domain::pending::LeafPayload;
use alloy::primitives::U256;
use chain_types::decode::{self, DecodedEvent, DepositNote};
use crypto::note::commitment_from_inner;
use database::models::RawEventRow;
use shared::entities::EventKind;
use std::collections::HashMap;

/// The two leaves one deposit mints, in the order `flushBatch` inserts them:
/// the depositor's note, then the note paying whoever flushed it.
///
/// The order is the leaf order the tree commits, so swapping them assigns both
/// notes the wrong `leaf_index` and every Merkle proof built against them fails.
#[derive(Clone)]
pub struct EscrowedLeaves {
    pub principal: LeafPayload,
    pub fee: LeafPayload,
}

/// Maps a deposit id to that deposit's two leaves.
pub type EscrowedMap = HashMap<U256, EscrowedLeaves>;

/// The deposit ids a window's `DepositFlushed` rows refer to, sorted and
/// deduped, as the `topics[2] = ANY($3)` lookup wants them: 32-byte big-endian
/// words straight off the log.
///
/// `DepositFlushed` topic 1 is the deposit id.
pub fn flushed_deposit_ids(rows: &[RawEventRow]) -> Vec<Vec<u8>> {
    let flushed = EventKind::DepositFlushed.as_i16();
    let mut ids: Vec<Vec<u8>> = rows
        .iter()
        .filter(|r| r.event_kind == flushed)
        .filter_map(|r| r.topics.get(1).cloned())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The deposit id and the two leaves one stored `DepositEscrowed` row published,
/// or `None` when the row does not decode as one.
pub fn decode_escrowed(row: &RawEventRow) -> Option<(U256, EscrowedLeaves)> {
    let decoded = decode::decode(EventKind::DepositEscrowed, &row.topics, &row.data).ok()?;
    let DecodedEvent::DepositEscrowed { id, note, fee, .. } = decoded.into_iter().next()? else {
        return None;
    };

    // The fee note is a leaf with its own FMD payload and is scanned like any
    // other; the relayer detects its own note by trial decryption, as a wallet
    // does.
    let leaves = EscrowedLeaves {
        principal: leaf_payload(note)?,
        fee: leaf_payload(fee)?,
    };
    Some((id, leaves))
}

/// The leaf one deposit note becomes. The event carries `inner`; the leaf is
/// `crypto::note::commitment_from_inner`.
fn leaf_payload(note: DepositNote) -> Option<LeafPayload> {
    Some(LeafPayload {
        cm: commitment_from_inner(note.asset_id, note.value, &note.inner.0).ok()?,
        clue_rx: note.clue_rx,
        clue_ry: note.clue_ry,
        eph_pub_x: note.eph_pub_x,
        eph_pub_y: note.eph_pub_y,
        ciphertext: note.ciphertext,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{B256, Bytes};
    use alloy::sol_types::SolEvent;
    use chain_types::abi::DepositFlushed;
    use crypto::tree::Field;

    fn row(id: i64, kind: EventKind, topics: Vec<Vec<u8>>, data: Vec<u8>) -> RawEventRow {
        RawEventRow {
            id,
            chain_id: 1,
            block_number: 10,
            block_hash: vec![0xaa; 32],
            block_ts: 1_700_000_000,
            tx_hash: vec![0x01; 32],
            log_index: id as i32,
            event_kind: kind.as_i16(),
            topics,
            data,
        }
    }

    fn flushed_row(id: i64, deposit_id: u64) -> RawEventRow {
        let log = DepositFlushed {
            id: U256::from(deposit_id),
            inner: B256::repeat_byte(0x11),
        }
        .encode_log_data();
        row(
            id,
            EventKind::DepositFlushed,
            log.topics().iter().map(|t| t.0.to_vec()).collect(),
            log.data.to_vec(),
        )
    }

    #[test]
    fn flushed_deposit_ids_reads_the_indexed_topic_and_dedupes() {
        let rows = [
            flushed_row(1, 9),
            // Not a flush: must not contribute a lookup key.
            row(2, EventKind::RootAdvanced, vec![vec![0xff; 32]], Vec::new()),
            flushed_row(3, 7),
            // The same deposit flushed twice in one window is one lookup.
            flushed_row(4, 9),
        ];

        let ids = flushed_deposit_ids(&rows);

        // topics[0] is the event signature; the id is topics[1], 32 bytes BE.
        let expect = |n: u64| U256::from(n).to_be_bytes::<32>().to_vec();
        assert_eq!(ids.len(), 2, "sorted and deduped");
        assert!(ids.contains(&expect(7)) && ids.contains(&expect(9)));
    }

    #[test]
    fn flushed_deposit_ids_tolerates_a_log_with_no_indexed_topic() {
        // A malformed row must be skipped rather than panic on `topics[1]`.
        let rows = [row(1, EventKind::DepositFlushed, Vec::new(), Vec::new())];

        assert!(flushed_deposit_ids(&rows).is_empty());
    }

    /// A deposit note's public `(asset, value)` and its `inner`.
    struct Note {
        asset: u64,
        value: u64,
        inner: B256,
    }

    const PRINCIPAL: Note = Note {
        asset: 7,
        value: 10,
        inner: B256::repeat_byte(0x11),
    };
    const FEE: Note = Note {
        asset: 9,
        value: 30,
        inner: B256::repeat_byte(0x12),
    };

    fn leaf(note: &Note) -> Field {
        commitment_from_inner(note.asset, note.value, &note.inner.0).expect("poseidon")
    }

    fn escrowed_row(principal: &Note, fee: &Note) -> RawEventRow {
        let ev = chain_types::abi::DepositEscrowed {
            id: U256::from(42u64),
            payer: Default::default(),
            recipient: Default::default(),
            publicAssetId: principal.asset,
            publicIn: principal.value,
            feeBpsAtSubmit: 0,
            inner: principal.inner,
            clueRx: U256::from(1u64),
            clueRy: U256::from(2u64),
            ephPubX: U256::ZERO,
            ephPubY: U256::ZERO,
            ciphertext: Bytes::from(vec![0x00, 0x07]),
            feeAssetId: fee.asset,
            feeIn: fee.value,
            feeInner: fee.inner,
            feeClueRx: U256::from(3u64),
            feeClueRy: U256::from(4u64),
            feeEphPubX: U256::ZERO,
            feeEphPubY: U256::ZERO,
            feeCiphertext: Bytes::from(vec![0x00, 0x09]),
            pulled: U256::ZERO,
        };
        let log = ev.encode_log_data();
        row(
            1,
            EventKind::DepositEscrowed,
            log.topics().iter().map(|t| t.0.to_vec()).collect(),
            log.data.to_vec(),
        )
    }

    #[test]
    fn decode_escrowed_keys_the_payload_by_deposit_id() {
        let stored = escrowed_row(&PRINCIPAL, &FEE);

        let (id, payload) = decode_escrowed(&stored).expect("round-trips");

        assert_eq!(id, U256::from(42u64));
        assert_eq!(payload.principal.ciphertext, vec![0x00, 0x07]);
        // The fee leaf is carried in the same event and must land in the second
        // slot: the tree inserts it after the principal, so a swap gives both
        // notes the wrong leaf index.
        assert_eq!(payload.fee.ciphertext, vec![0x00, 0x09]);
    }

    #[test]
    fn decode_escrowed_computes_each_leaf_from_its_own_asset_value_and_inner() {
        let stored = escrowed_row(&PRINCIPAL, &FEE);

        let (_, payload) = decode_escrowed(&stored).expect("round-trips");

        assert_eq!(payload.principal.cm, leaf(&PRINCIPAL));
        assert_eq!(payload.fee.cm, leaf(&FEE));
    }

    #[test]
    fn decode_escrowed_puts_an_unpaid_fee_note_under_asset_zero() {
        let unpaid = Note {
            asset: 0,
            value: 0,
            ..FEE
        };
        let stored = escrowed_row(&PRINCIPAL, &unpaid);

        let (_, payload) = decode_escrowed(&stored).expect("round-trips");

        assert_eq!(payload.fee.cm, leaf(&unpaid));
        assert_ne!(payload.fee.cm, leaf(&FEE));
    }

    #[test]
    fn decode_escrowed_rejects_a_row_that_is_not_a_deposit_escrowed() {
        let junk = row(
            1,
            EventKind::DepositEscrowed,
            vec![vec![0xff; 32]],
            Vec::new(),
        );

        assert!(decode_escrowed(&junk).is_none());
    }
}
