use alloy::primitives::{Address, B256, U256};
use alloy::sol_types::SolEvent;
use chain_types::abi::{
    AssetFeeSet, AssetMoved, AssetRegistered, DepositCanceled, DepositEscrowed, DepositFlushed,
    NotePayload, ProposalCreated, ProposalQueued, ProposalQuorumVoteDeadline, RootAdvanced,
    VoteCast, VoteCastWithParams,
};
use chain_types::decode::{DecodedEvent, decode, event_kind_from_topic0, known_signatures};
use shared::entities::EventKind;

fn topic_bytes(t: &B256) -> Vec<u8> {
    t.0.to_vec()
}

/// Encode `ev` as its log, decode it back as `kind`, and return the one event
/// that must come out: every kind decodes to exactly one.
fn roundtrip<E: SolEvent>(kind: EventKind, ev: &E) -> DecodedEvent {
    let log = ev.encode_log_data();
    let topics: Vec<Vec<u8>> = log.topics().iter().map(topic_bytes).collect();
    let mut decoded = decode(kind, &topics, &log.data).expect("decode");
    assert_eq!(decoded.len(), 1, "one event per log");
    decoded.remove(0)
}

#[test]
fn known_signatures_unique() {
    let sigs = known_signatures();
    let mut sorted = sigs.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), sigs.len());
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
    // A deposit leg, at a scale of 1e6: the base-unit and circuit-unit figures
    // differ, so a decoder that crossed them would not survive this.
    let ev = AssetMoved {
        assetId: 7,
        token,
        inAmount: U256::from(1_000_000u64),
        outAmount: U256::ZERO,
        publicIn: 1,
        publicOut: 0,
    };
    match &roundtrip(EventKind::AssetMoved, &ev) {
        DecodedEvent::AssetMoved {
            asset_id,
            token: t,
            in_amount,
            out_amount,
            public_in,
            public_out,
        } => {
            assert_eq!(*asset_id, 7);
            assert_eq!(*t, token);
            assert_eq!(*in_amount, U256::from(1_000_000u64));
            assert_eq!(*out_amount, U256::ZERO);
            assert_eq!(*public_in, 1);
            assert_eq!(*public_out, 0);
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
    };
    match &roundtrip(EventKind::NoteCreated, &ev) {
        DecodedEvent::NoteCreated {
            cm: c,
            clue_rx,
            clue_ry,
            eph_pub_x,
            eph_pub_y,
            ciphertext,
        } => {
            assert_eq!(*c, cm);
            assert_eq!(*clue_rx, U256::from(111u64));
            assert_eq!(*clue_ry, U256::from(222u64));
            assert_eq!(*eph_pub_x, U256::from(333u64));
            assert_eq!(*eph_pub_y, U256::from(444u64));
            assert_eq!(*ciphertext, ct);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn deposit_escrowed_roundtrip() {
    let payer = Address::repeat_byte(0x01);
    let recipient = Address::repeat_byte(0x02);
    let inner_v = B256::repeat_byte(0xef);
    let ct = vec![0x00, 0x09, 0x77, 0x88];
    let fee_inner_v = B256::repeat_byte(0xab);
    let fee_ct = vec![0x11, 0x22, 0x33];
    // A yield escrow's refund cap, in token base units: wider than `u128`, so a
    // decoder that narrowed the word would not survive this.
    let pulled_v = (U256::from(1u8) << 200) + U256::from(4001u64);

    let ev = DepositEscrowed {
        id: U256::from(9u64),
        payer,
        recipient,
        publicAssetId: 3,
        publicIn: 250_000,
        feeBpsAtSubmit: 30,
        inner: inner_v,
        clueRx: U256::from(11u64),
        clueRy: U256::from(12u64),
        ephPubX: U256::from(13u64),
        ephPubY: U256::from(14u64),
        ciphertext: ct.clone().into(),
        feeAssetId: 7,
        feeIn: 500,
        feeInner: fee_inner_v,
        feeClueRx: U256::from(21u64),
        feeClueRy: U256::from(22u64),
        feeEphPubX: U256::from(23u64),
        feeEphPubY: U256::from(24u64),
        feeCiphertext: fee_ct.clone().into(),
        pulled: pulled_v,
    };
    match &roundtrip(EventKind::DepositEscrowed, &ev) {
        DecodedEvent::DepositEscrowed {
            id,
            payer: p,
            recipient: r,
            fee_bps_at_submit,
            note,
            fee,
            pulled,
        } => {
            assert_eq!(*id, U256::from(9u64));
            assert_eq!(*p, payer);
            assert_eq!(*r, recipient);
            assert_eq!(*fee_bps_at_submit, 30);
            assert_eq!(note.asset_id, 3);
            assert_eq!(note.value, 250_000);
            assert_eq!(note.inner, inner_v);
            assert_eq!(note.clue_rx, U256::from(11u64));
            assert_eq!(note.clue_ry, U256::from(12u64));
            assert_eq!(note.eph_pub_x, U256::from(13u64));
            assert_eq!(note.eph_pub_y, U256::from(14u64));
            assert_eq!(note.ciphertext, ct);
            // The fee note must survive the round trip intact: it is digest
            // preimage, so one dropped field makes the deposit unflushable rather
            // than merely mispriced.
            // Distinct from the depositor's asset (3): a cross-asset fee note.
            assert_eq!(fee.asset_id, 7);
            assert_eq!(fee.value, 500);
            assert_eq!(fee.inner, fee_inner_v);
            assert_eq!(fee.clue_rx, U256::from(21u64));
            assert_eq!(fee.clue_ry, U256::from(22u64));
            assert_eq!(fee.eph_pub_x, U256::from(23u64));
            assert_eq!(fee.eph_pub_y, U256::from(24u64));
            assert_eq!(fee.ciphertext, fee_ct);
            // The last head word, past both `bytes` offsets. Digest preimage
            // too: a flush replaying any other value reverts `DigestMismatch`.
            assert_eq!(*pulled, pulled_v);
        }
        _ => panic!("wrong variant"),
    }
}

/// A two-token cancel: the deposit token and the fee token are refunded
/// separately, and both amounts plus the fee asset must survive decoding.
#[test]
fn deposit_canceled_roundtrip() {
    let payer = Address::repeat_byte(0x0c);
    let ev = DepositCanceled {
        id: U256::from(11u64),
        payer,
        refunded: U256::from(1_002_500u64),
        feeAssetId: 5,
        feeRefunded: U256::from(700u64),
    };
    match &roundtrip(EventKind::DepositCanceled, &ev) {
        DecodedEvent::DepositCanceled {
            id,
            payer: p,
            refunded,
            fee_asset_id,
            fee_refunded,
        } => {
            assert_eq!(*id, U256::from(11u64));
            assert_eq!(*p, payer);
            assert_eq!(*refunded, U256::from(1_002_500u64));
            assert_eq!(*fee_asset_id, 5);
            assert_eq!(*fee_refunded, U256::from(700u64));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn deposit_flushed_roundtrip() {
    let inner_v = B256::repeat_byte(0x5a);
    let ev = DepositFlushed {
        id: U256::from(4u64),
        inner: inner_v,
    };
    match &roundtrip(EventKind::DepositFlushed, &ev) {
        DecodedEvent::DepositFlushed { id, inner } => {
            assert_eq!(*id, U256::from(4u64));
            assert_eq!(*inner, inner_v);
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
    match &roundtrip(EventKind::AssetFeeSet, &ev) {
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
    match &roundtrip(EventKind::AssetRegistered, &ev) {
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
    match &roundtrip(EventKind::RootAdvanced, &ev) {
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

#[test]
fn proposal_created_roundtrip() {
    let ev = ProposalCreated {
        proposalId: U256::from(123u64),
        proposer: Address::repeat_byte(0x01),
        targets: vec![Address::repeat_byte(0x02), Address::repeat_byte(0x03)],
        values: vec![U256::ZERO, U256::from(5u64)],
        signatures: vec![String::new(), String::new()],
        calldatas: vec![vec![0xde, 0xad].into(), vec![].into()],
        voteStart: U256::from(1_000u64),
        voteEnd: U256::from(1_300u64),
        description: "# Title\n\nbody".into(),
    };
    match roundtrip(EventKind::ProposalCreated, &ev) {
        DecodedEvent::ProposalCreated {
            proposal_id,
            proposer,
            targets,
            values,
            signatures,
            calldatas,
            vote_start,
            vote_end,
            description,
        } => {
            assert_eq!(proposal_id, U256::from(123u64));
            assert_eq!(proposer, Address::repeat_byte(0x01));
            assert_eq!(targets.len(), 2);
            assert_eq!(values[1], U256::from(5u64));
            assert_eq!(signatures, vec![String::new(), String::new()]);
            assert_eq!(calldatas, vec![vec![0xde, 0xad], vec![]]);
            assert_eq!(vote_start, U256::from(1_000u64));
            assert_eq!(vote_end, U256::from(1_300u64));
            assert_eq!(description, "# Title\n\nbody");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

/// Both vote events land on one variant; only `params` tells them apart.
#[test]
fn vote_cast_and_vote_cast_with_params_roundtrip() {
    let voter = Address::repeat_byte(0x09);
    let plain = VoteCast {
        voter,
        proposalId: U256::from(1u64),
        support: 2,
        weight: U256::from(10u64),
        reason: "why".into(),
    };
    match roundtrip(EventKind::VoteCast, &plain) {
        DecodedEvent::VoteCast {
            voter: v,
            support,
            weight,
            reason,
            params,
            ..
        } => {
            assert_eq!(v, voter, "the indexed voter is read from topics");
            assert_eq!(support, 2);
            assert_eq!(weight, U256::from(10u64));
            assert_eq!(reason, "why");
            assert_eq!(params, None);
        }
        other => panic!("wrong variant: {other:?}"),
    }

    let with = VoteCastWithParams {
        voter,
        proposalId: U256::from(1u64),
        support: 1,
        weight: U256::from(10u64),
        reason: String::new(),
        params: vec![0x01].into(),
    };
    match roundtrip(EventKind::VoteCastWithParams, &with) {
        DecodedEvent::VoteCast { params, .. } => assert_eq!(params, Some(vec![0x01])),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn proposal_lifecycle_events_roundtrip() {
    let ev = ProposalQuorumVoteDeadline {
        proposalId: U256::from(7u64),
        quorumVoteDeadline: U256::from(99u64),
    };
    match roundtrip(EventKind::ProposalQuorumVoteDeadline, &ev) {
        DecodedEvent::ProposalQuorumVoteDeadline {
            proposal_id,
            quorum_vote_deadline,
        } => {
            assert_eq!(proposal_id, U256::from(7u64));
            assert_eq!(quorum_vote_deadline, U256::from(99u64));
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let ev = ProposalQueued {
        proposalId: U256::from(7u64),
        etaSeconds: U256::from(500u64),
    };
    match roundtrip(EventKind::ProposalQueued, &ev) {
        DecodedEvent::ProposalQueued { eta_seconds, .. } => {
            assert_eq!(eta_seconds, U256::from(500u64))
        }
        other => panic!("wrong variant: {other:?}"),
    }
}
