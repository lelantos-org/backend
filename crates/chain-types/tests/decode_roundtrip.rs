use alloy::primitives::{Address, B256, U256};
use alloy::sol_types::SolEvent;
use chain_types::abi::{
    AssetFeeSet, AssetMoved, AssetRegistered, DepositEscrowed, DepositFlushed, NotePayload,
    RootAdvanced,
};
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
    assert_eq!(sorted.len(), 9);
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
        event_kind_from_topic0(&AssetFeeSet::SIGNATURE_HASH),
        Some(EventKind::AssetFeeSet)
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
fn note_payload_roundtrip() {
    let cm = B256::repeat_byte(0xab);
    let ct = vec![0x00, 0x05, 0x42, 0x42];

    let ev = NotePayload {
        cm,
        clueRx: U256::from(111u64),
        clueRy: U256::from(222u64),
        ephPubX: U256::from(333u64),
        ephPubY: U256::from(444u64),
        ciphertext: ct.clone().into(),
        cvDepX: U256::from(1001u64),
        cvDepY: U256::from(1002u64),
    };
    let log = ev.encode_log_data();
    let topics: Vec<Vec<u8>> = log.topics().iter().map(topic_bytes).collect();
    let data = log.data.to_vec();

    let decoded = decode(EventKind::NoteCreated, &topics, &data).expect("decode");
    assert_eq!(decoded.len(), 1, "one log per output leaf");

    match &decoded[0] {
        DecodedEvent::NoteCreated {
            cm: c,
            clue_rx,
            clue_ry,
            eph_pub_x,
            eph_pub_y,
            ciphertext,
            cv_dep_x,
            cv_dep_y,
        } => {
            assert_eq!(*c, cm);
            assert_eq!(*clue_rx, U256::from(111u64));
            assert_eq!(*clue_ry, U256::from(222u64));
            assert_eq!(*eph_pub_x, U256::from(333u64));
            assert_eq!(*eph_pub_y, U256::from(444u64));
            assert_eq!(*ciphertext, ct);
            assert_eq!(*cv_dep_x, U256::from(1001u64));
            assert_eq!(*cv_dep_y, U256::from(1002u64));
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn deposit_escrowed_roundtrip() {
    let payer = Address::repeat_byte(0x01);
    let recipient = Address::repeat_byte(0x02);
    let cm = B256::repeat_byte(0xef);
    let ct = vec![0x00, 0x09, 0x77, 0x88];
    let fee_cm_v = B256::repeat_byte(0xab);
    let fee_ct = vec![0x11, 0x22, 0x33];

    let ev = DepositEscrowed {
        id: U256::from(9u64),
        payer,
        recipient,
        publicAssetId: 3,
        publicIn: 250_000,
        feeBpsAtSubmit: 30,
        cm,
        cvDepX: U256::from(2001u64),
        cvDepY: U256::from(2002u64),
        rcv: U256::from(2003u64),
        clueRx: U256::from(11u64),
        clueRy: U256::from(12u64),
        ephPubX: U256::from(13u64),
        ephPubY: U256::from(14u64),
        ciphertext: ct.clone().into(),
        feeIn: 500,
        feeCm: fee_cm_v,
        feeCvDepX: U256::from(3001u64),
        feeCvDepY: U256::from(3002u64),
        feeRcv: U256::from(3003u64),
        feeClueRx: U256::from(21u64),
        feeClueRy: U256::from(22u64),
        feeEphPubX: U256::from(23u64),
        feeEphPubY: U256::from(24u64),
        feeCiphertext: fee_ct.clone().into(),
    };
    let log = ev.encode_log_data();
    let topics: Vec<Vec<u8>> = log.topics().iter().map(topic_bytes).collect();
    let data = log.data.to_vec();

    let decoded = decode(EventKind::DepositEscrowed, &topics, &data).expect("decode");
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        DecodedEvent::DepositEscrowed {
            id,
            payer: p,
            recipient: r,
            public_asset_id,
            public_in,
            fee_bps_at_submit,
            cm: c,
            cv_dep_x,
            cv_dep_y,
            rcv,
            clue_rx,
            clue_ry,
            eph_pub_x,
            eph_pub_y,
            ciphertext,
            fee,
        } => {
            assert_eq!(*id, U256::from(9u64));
            assert_eq!(*p, payer);
            assert_eq!(*r, recipient);
            assert_eq!(*public_asset_id, 3);
            assert_eq!(*public_in, 250_000);
            assert_eq!(*fee_bps_at_submit, 30);
            assert_eq!(*c, cm);
            assert_eq!(*cv_dep_x, U256::from(2001u64));
            assert_eq!(*cv_dep_y, U256::from(2002u64));
            assert_eq!(*rcv, U256::from(2003u64));
            assert_eq!(*clue_rx, U256::from(11u64));
            assert_eq!(*clue_ry, U256::from(12u64));
            assert_eq!(*eph_pub_x, U256::from(13u64));
            assert_eq!(*eph_pub_y, U256::from(14u64));
            assert_eq!(*ciphertext, ct);
            // The fee note must survive the round trip intact: it is digest
            // preimage, so one dropped field makes the deposit unflushable rather
            // than merely mispriced.
            assert_eq!(fee.fee_in, 500);
            assert_eq!(fee.cm, fee_cm_v);
            assert_eq!(fee.cv_dep_x, U256::from(3001u64));
            assert_eq!(fee.cv_dep_y, U256::from(3002u64));
            assert_eq!(fee.rcv, U256::from(3003u64));
            assert_eq!(fee.clue_rx, U256::from(21u64));
            assert_eq!(fee.clue_ry, U256::from(22u64));
            assert_eq!(fee.eph_pub_x, U256::from(23u64));
            assert_eq!(fee.eph_pub_y, U256::from(24u64));
            assert_eq!(fee.ciphertext, fee_ct);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn deposit_flushed_roundtrip() {
    let cm = B256::repeat_byte(0x5a);
    let ev = DepositFlushed {
        id: U256::from(4u64),
        cm,
    };
    let log = ev.encode_log_data();
    let topics: Vec<Vec<u8>> = log.topics().iter().map(topic_bytes).collect();
    let data = log.data.to_vec();

    let decoded = decode(EventKind::DepositFlushed, &topics, &data).expect("decode");
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        DecodedEvent::DepositFlushed { id, cm: c } => {
            assert_eq!(*id, U256::from(4u64));
            assert_eq!(*c, cm);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn asset_fee_set_roundtrip() {
    // A zero deposit rate beside a non-zero withdraw rate: the asymmetric
    // shape the contract exists to express, and the case a decoder that
    // treated 0 as "absent" would corrupt.
    let ev = AssetFeeSet {
        assetId: 7,
        depositBps: 0,
        withdrawBps: 20,
    };
    let log = ev.encode_log_data();
    let topics: Vec<Vec<u8>> = log.topics().iter().map(topic_bytes).collect();
    let data = log.data.to_vec();

    let decoded = decode(EventKind::AssetFeeSet, &topics, &data).expect("decode");
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        DecodedEvent::AssetFeeSet {
            asset_id,
            deposit_bps,
            withdraw_bps,
        } => {
            assert_eq!(*asset_id, 7);
            assert_eq!(*deposit_bps, 0);
            assert_eq!(*withdraw_bps, 20);
        }
        other => panic!("wrong variant: {other:?}"),
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
