use alloy::primitives::{Address, B256, U256};
use alloy::sol_types::SolEvent;
use chain_types::abi::{AssetMoved, AssetRegistered, NotePayload, RootAdvanced};
use chain_types::decode::{DecodedEvent, decode, event_kind_from_topic0, known_signatures};
use shared::entities::EventKind;

fn topic_bytes(t: &B256) -> Vec<u8> {
    t.0.to_vec()
}

#[test]
fn known_signatures_unique() {
    let sigs = known_signatures();
    let mut sorted = sigs.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 8);
}

#[test]
fn topic0_to_event_kind_maps() {
    assert_eq!(
        event_kind_from_topic0(&NotePayload::SIGNATURE_HASH),
        Some(EventKind::NoteCreated)
    );
    assert_eq!(
        event_kind_from_topic0(&AssetRegistered::SIGNATURE_HASH),
        Some(EventKind::AssetRegistered)
    );
    assert_eq!(
        event_kind_from_topic0(&RootAdvanced::SIGNATURE_HASH),
        Some(EventKind::RootAdvanced)
    );
    assert_eq!(
        event_kind_from_topic0(&AssetMoved::SIGNATURE_HASH),
        Some(EventKind::AssetMoved)
    );
    assert_eq!(event_kind_from_topic0(&B256::ZERO), None);
}

#[test]
fn asset_moved_roundtrip() {
    let token = Address::repeat_byte(0x44);
    let ev = AssetMoved {
        assetId: 7,
        token,
        inAmount: U256::from(1_000_000u64),
        outAmount: U256::ZERO,
    };
    let log = ev.encode_log_data();
    let topics: Vec<Vec<u8>> = log.topics().iter().map(topic_bytes).collect();
    let data = log.data.to_vec();

    let decoded = decode(EventKind::AssetMoved, &topics, &data).expect("decode");
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        DecodedEvent::AssetMoved {
            asset_id,
            token: t,
            in_amount,
            out_amount,
        } => {
            assert_eq!(*asset_id, 7);
            assert_eq!(*t, token);
            assert_eq!(*in_amount, U256::from(1_000_000u64));
            assert_eq!(*out_amount, U256::ZERO);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn notes_created_fanout_roundtrip() {
    let cm0 = B256::repeat_byte(0xab);
    let cm1 = B256::repeat_byte(0xcd);
    let ct0 = vec![0x00, 0x05, 0x42, 0x42];
    let ct1 = vec![0x00, 0x07, 0x99];

    let ev = NotePayload {
        cm0,
        cm1,
        clueRx0: U256::from(111u64),
        clueRy0: U256::from(222u64),
        ephPubX0: U256::from(333u64),
        ephPubY0: U256::from(444u64),
        ciphertext0: ct0.clone().into(),
        clueRx1: U256::from(555u64),
        clueRy1: U256::from(666u64),
        ephPubX1: U256::from(777u64),
        ephPubY1: U256::from(888u64),
        ciphertext1: ct1.clone().into(),
        cvDep0X: U256::from(1001u64),
        cvDep0Y: U256::from(1002u64),
        cvDep1X: U256::from(1003u64),
        cvDep1Y: U256::from(1004u64),
    };
    let log = ev.encode_log_data();
    let topics: Vec<Vec<u8>> = log.topics().iter().map(topic_bytes).collect();
    let data = log.data.to_vec();

    let decoded = decode(EventKind::NoteCreated, &topics, &data).expect("decode");
    assert_eq!(decoded.len(), 2, "fan-out yields two NoteCreated entries");

    match &decoded[0] {
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
            assert_eq!(*cm, cm0);
            assert_eq!(*clue_rx, U256::from(111u64));
            assert_eq!(*clue_ry, U256::from(222u64));
            assert_eq!(*eph_pub_x, U256::from(333u64));
            assert_eq!(*eph_pub_y, U256::from(444u64));
            assert_eq!(*ciphertext, ct0);
            assert_eq!(*cv_dep_x, U256::from(1001u64));
            assert_eq!(*cv_dep_y, U256::from(1002u64));
        }
        _ => panic!("wrong variant at idx 0"),
    }
    match &decoded[1] {
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
            assert_eq!(*cm, cm1);
            assert_eq!(*clue_rx, U256::from(555u64));
            assert_eq!(*clue_ry, U256::from(666u64));
            assert_eq!(*eph_pub_x, U256::from(777u64));
            assert_eq!(*eph_pub_y, U256::from(888u64));
            assert_eq!(*ciphertext, ct1);
            assert_eq!(*cv_dep_x, U256::from(1003u64));
            assert_eq!(*cv_dep_y, U256::from(1004u64));
        }
        _ => panic!("wrong variant at idx 1"),
    }
}

#[test]
fn asset_registered_roundtrip() {
    let token = Address::repeat_byte(0x33);
    let ev = AssetRegistered {
        assetId: 42,
        token,
        scale: U256::from(1_000_000u64),
    };
    let log = ev.encode_log_data();
    let topics: Vec<Vec<u8>> = log.topics().iter().map(topic_bytes).collect();
    let data = log.data.to_vec();

    let decoded = decode(EventKind::AssetRegistered, &topics, &data).expect("decode");
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        DecodedEvent::AssetRegistered {
            asset_id,
            token: t,
            scale,
        } => {
            assert_eq!(*asset_id, 42);
            assert_eq!(*t, token);
            assert_eq!(*scale, U256::from(1_000_000u64));
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn root_advanced_roundtrip() {
    let old_root = B256::repeat_byte(0x11);
    let new_root = B256::repeat_byte(0x22);

    let ev = RootAdvanced {
        startIndex: 4,
        inserted: 2,
        oldRoot: old_root,
        newRoot: new_root,
    };
    let log = ev.encode_log_data();
    let topics: Vec<Vec<u8>> = log.topics().iter().map(topic_bytes).collect();
    let data = log.data.to_vec();

    let decoded = decode(EventKind::RootAdvanced, &topics, &data).expect("decode");
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        DecodedEvent::RootAdvanced {
            start_index,
            inserted,
            old_root: o,
            new_root: n,
        } => {
            assert_eq!(*start_index, 4);
            assert_eq!(*inserted, 2);
            assert_eq!(*o, old_root);
            assert_eq!(*n, new_root);
        }
        _ => panic!("wrong variant"),
    }
}
