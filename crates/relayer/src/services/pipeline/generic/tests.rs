//! Generic payload validation, the intent hash and the bundled call, against
//! hand-built payloads.

use super::validate::{build_generic_args, generic_intent_hash, validate_generic};
use super::*;
use crate::domain::dto::{
    DepositRequestDto, GenericBlob, GenericCallDto, GenericOutputDto, PubInputsDto, TRANSACT_IN,
};
use crate::domain::error::AppError;
use crate::domain::field::BN254_R;
use crate::services::pipeline::fixtures::{
    CHAIN_ID, binding, bundler, fake_deposit, fake_proof, fake_pub_inputs, one_aux, wrapper,
};
use alloy::primitives::{FixedBytes, Selector};
use alloy::sol_types::SolValue;
use std::array;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_MIN_GAS: u64 = 2_000_000;

/// The one contract and selector the default policy admits.
fn target() -> Address {
    Address::repeat_byte(0xAA)
}

const SELECTOR: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];

/// Neither the wrapper, the Bundler nor a listed target.
const OTHER: &str = "0x000000000000000000000000000000000000dead";

fn policy(allowed: &[(Address, [u8; 4])]) -> GenericPolicy {
    GenericPolicy {
        binding: binding(),
        allowed_calls: allowed
            .iter()
            .map(|(t, s)| (*t, Selector::from(*s)))
            .collect(),
        max_min_gas: MAX_MIN_GAS,
    }
}

fn call(target: Address, data: &str) -> GenericCallDto {
    GenericCallDto {
        target: target.to_string(),
        value: "0".into(),
        data: data.into(),
    }
}

fn output(asset_id: u64, min_out: &str) -> GenericOutputDto {
    GenericOutputDto {
        min_out: min_out.into(),
        deposit: DepositRequestDto {
            public_asset_id: asset_id,
            ..fake_deposit(&wrapper().to_string(), 400)
        },
        aux: one_aux(),
        fee_aux: one_aux(),
    }
}

/// Point the proof at the payload's current terms, as a wallet does last.
fn bind(p: &mut SubmitGenericPayload) {
    p.pub_inputs.intent_hash = generic_intent_hash(&build_generic_args(p).unwrap()).to_string();
}

/// A two-call, two-output payload whose proof commits to its own terms.
fn fake_payload() -> SubmitGenericPayload {
    let w = wrapper().to_string();
    let mut p = SubmitGenericPayload {
        chain_id: CHAIN_ID,
        proof: fake_proof(),
        pub_inputs: PubInputsDto {
            payer: bundler().to_string(),
            relayer: w.clone(),
            ..fake_pub_inputs(&w, 1_000)
        },
        aux: array::from_fn(|_| one_aux()),
        generic: GenericBlob {
            amount_in: "1000".into(),
            calls: vec![call(target(), "0xdeadbeef01"), call(target(), "0xdeadbeef")],
            outputs: vec![output(2, "400"), output(3, "10")],
            deadline: "1900000000".into(),
            min_gas: "600000".into(),
            refund_to: "0x000000000000000000000000000000000000cafe".into(),
            surplus_to: "0x000000000000000000000000000000000000f00d".into(),
            refund_d: DepositRequestDto {
                public_asset_id: 1,
                inner: format!("0x{:0>64}", "7"),
                ..fake_deposit(&w, 995)
            },
            refund_aux_d: one_aux(),
            refund_fee_aux_d: one_aux(),
        },
    };
    bind(&mut p);
    p
}

type Mutation = fn(&mut SubmitGenericPayload);

/// A well-formed payload plus a mutation, against `policy`.
fn checked_against(
    policy: &GenericPolicy,
    mutate: impl FnOnce(&mut SubmitGenericPayload),
) -> AppResult<()> {
    let mut p = fake_payload();
    mutate(&mut p);
    validate_generic(&p, policy).map(|_| ())
}

/// The same, against a policy admitting `(target(), SELECTOR)`.
fn checked(mutate: impl FnOnce(&mut SubmitGenericPayload)) -> AppResult<()> {
    checked_against(&policy(&[(target(), SELECTOR)]), mutate)
}

/// `result` must be a 400 naming `needle`.
fn assert_rejected(case: &str, needle: &str, result: AppResult<()>) {
    match result {
        Err(AppError::BadRequest(m)) if m.contains(needle) => {}
        other => panic!("{case}: expected a 400 naming {needle}, got {other:?}"),
    }
}

/// Each row is a payload [`checked`] must refuse: what is wrong with it, what
/// the 400 must name, and the mutation that makes it so.
fn each_rejected<const N: usize>(cases: [(&str, &str, Mutation); N]) {
    for (case, needle, mutate) in cases {
        assert_rejected(case, needle, checked(mutate));
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn validate_accepts_a_well_formed_payload_up_to_each_bound() {
    let cases: [(&str, Mutation); 4] = [
        ("as built", |_| {}),
        ("sixteen calls", |p| {
            p.generic.calls = vec![call(target(), "0xdeadbeef"); 16];
            bind(p);
        }),
        ("four outputs", |p| {
            p.generic.outputs = (2..6).map(|asset| output(asset, "1")).collect();
            bind(p);
        }),
        ("minGas at the configured maximum", |p| {
            p.generic.min_gas = MAX_MIN_GAS.to_string();
            bind(p);
        }),
    ];
    for (case, mutate) in cases {
        checked(mutate).unwrap_or_else(|e| panic!("{case}: {e}"));
    }
}

#[test]
fn validate_returns_min_gas_as_gas_units() {
    let (args, min_gas) =
        validate_generic(&fake_payload(), &policy(&[(target(), SELECTOR)])).unwrap();
    assert_eq!(min_gas, 600_000);
    assert_eq!(args.minGas, U256::from(600_000u64));
}

// --- allowlist ---

#[test]
fn validate_rejects_a_call_off_the_allowlist() {
    each_rejected([
        ("another target", "generic.calls[1]", |p| {
            p.generic.calls[1].target = OTHER.into();
            bind(p);
        }),
        ("another selector", "generic.calls[0]", |p| {
            p.generic.calls[0].data = "0xdeadbee0".into();
            bind(p);
        }),
        ("no calldata", "generic.calls[0].data", |p| {
            p.generic.calls[0].data = "0x".into();
            bind(p);
        }),
        ("three bytes of calldata", "generic.calls[0].data", |p| {
            p.generic.calls[0].data = "0xdeadbe".into();
            bind(p);
        }),
    ]);
}

/// The pair is what is listed: a listed selector on another target, or another
/// selector on a listed target, is not.
#[test]
fn validate_matches_target_and_selector_together() {
    let other = Address::repeat_byte(0xBB);
    let policy = policy(&[(target(), SELECTOR), (other, [1, 2, 3, 4])]);
    checked_against(&policy, |p| {
        p.generic.calls[1] = call(other, "0x01020304ff");
        bind(p);
    })
    .unwrap();
    let err = checked_against(&policy, |p| {
        p.generic.calls[1] = call(other, "0xdeadbeef");
        bind(p);
    })
    .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)), "{err}");
}

#[test]
fn an_empty_allowlist_admits_only_zero_call_intents() {
    let none = policy(&[]);
    assert_rejected(
        "the payload as built",
        "generic.calls[0]",
        checked_against(&none, |_| {}),
    );
    checked_against(&none, |p| {
        p.generic.calls.clear();
        bind(p);
    })
    .unwrap();
}

// --- counts, amounts and minGas ---

#[test]
fn validate_rejects_counts_and_amounts_out_of_bounds() {
    each_rejected([
        ("seventeen calls", "generic.calls must hold", |p| {
            p.generic.calls = vec![call(target(), "0xdeadbeef"); 17];
            bind(p);
        }),
        ("no outputs", "generic.outputs must hold", |p| {
            p.generic.outputs.clear()
        }),
        ("five outputs", "generic.outputs must hold", |p| {
            p.generic.outputs = (2..7).map(|asset| output(asset, "1")).collect()
        }),
        ("zero amountIn", "generic.amountIn", |p| {
            p.generic.amount_in = "0".into()
        }),
        ("zero minOut", "generic.outputs[1].minOut", |p| {
            p.generic.outputs[1].min_out = "0".into()
        }),
        ("zero minGas", "generic.minGas", |p| {
            p.generic.min_gas = "0".into()
        }),
        (
            "minGas above the configured maximum",
            "generic.minGas",
            |p| p.generic.min_gas = (MAX_MIN_GAS + 1).to_string(),
        ),
        ("minGas wider than the u64 bound", "generic.minGas", |p| {
            p.generic.min_gas = U256::MAX.to_string()
        }),
    ]);
}

// --- escrowed deposits ---

#[test]
fn validate_rejects_a_deposit_the_wrapper_cannot_escrow() {
    each_rejected([
        (
            "an output the wrapper does not pay for",
            "generic.outputs[1].deposit.payer",
            |p| p.generic.outputs[1].deposit.payer = OTHER.into(),
        ),
        (
            "an output on another chain",
            "generic.outputs[0].deposit.chainId",
            |p| p.generic.outputs[0].deposit.chain_id = CHAIN_ID as u64 + 1,
        ),
        (
            "an output inner that is not a field element",
            "generic.outputs[0].deposit.inner",
            |p| p.generic.outputs[0].deposit.inner = BN254_R.to_string(),
        ),
        (
            "a refund the wrapper does not pay for",
            "generic.refundD.payer",
            |p| p.generic.refund_d.payer = OTHER.into(),
        ),
        ("a zero-value refund", "generic.refundD.publicIn", |p| {
            p.generic.refund_d.public_in = 0
        }),
    ]);
}

// --- receivers ---

#[test]
fn validate_rejects_a_receiver_that_cannot_move_funds() {
    type Set = fn(&mut SubmitGenericPayload, String);
    let fields: [(&str, Set); 2] = [
        ("generic.refundTo", |p, v| p.generic.refund_to = v),
        ("generic.surplusTo", |p, v| p.generic.surplus_to = v),
    ];
    for (field, set) in fields {
        for bad in [Address::ZERO, wrapper(), bundler()] {
            let result = checked(|p| set(p, bad.to_string()));
            assert_rejected(&bad.to_string(), field, result);
        }
    }
}

// --- leg-1 binding ---

#[test]
fn validate_rejects_a_proof_not_bound_to_this_wrapper_and_bundler() {
    each_rejected([
        ("nothing withdrawn", "publicOut", |p| {
            p.pub_inputs.public_out = 0
        }),
        ("another recipient", "pi.recipient", |p| {
            p.pub_inputs.recipient = OTHER.into()
        }),
        // The wrapper is the pool's `msg.sender`.
        ("another relayer", "pubInputs.relayer", |p| {
            p.pub_inputs.relayer = OTHER.into()
        }),
        // The wrapper lets only `pi.payer` call `execute`, and the Bundler is
        // its caller.
        ("a payer other than the Bundler", "pubInputs.payer", |p| {
            p.pub_inputs.payer = OTHER.into()
        }),
        ("another chain", "pubInputs.chainId", |p| {
            p.pub_inputs.chain_id = CHAIN_ID as u64 + 1
        }),
    ]);
}

// --- deadline and intent ---

#[test]
fn validate_rejects_a_deadline_expired_or_inside_the_margin() {
    each_rejected([
        ("expired", "generic.deadline", |p| {
            p.generic.deadline = "1".into()
        }),
        ("ten seconds away", "generic.deadline", |p| {
            p.generic.deadline = (now_secs() + 10).to_string();
            bind(p);
        }),
    ]);
}

/// Each bound term moves the hash, so a payload changed after binding is
/// refused, as is a hash that was never the terms'.
#[test]
fn validate_rejects_an_intent_hash_the_terms_do_not_produce() {
    let cases: [(&str, Mutation); 7] = [
        ("intentHash", |p| p.pub_inputs.intent_hash = "1".into()),
        ("surplusTo", |p| p.generic.surplus_to = OTHER.into()),
        ("minGas", |p| p.generic.min_gas = "600001".into()),
        ("call value", |p| p.generic.calls[0].value = "1".into()),
        ("call data", |p| {
            p.generic.calls[0].data = "0xdeadbeef02".into()
        }),
        ("minOut", |p| p.generic.outputs[0].min_out = "399".into()),
        ("refund aux", |p| {
            p.generic.refund_aux_d.ciphertext = "0x01".into()
        }),
    ];
    for (changed, mutate) in cases {
        assert_rejected(changed, "intentHash", checked(mutate));
    }
}

/// `amountIn` is outside the intent: the withdraw proof fixes the amount.
#[test]
fn amount_in_is_not_part_of_the_intent() {
    checked(|p| p.generic.amount_in = "999".into()).unwrap();
}

/// `GenericCallWrapperBindingTest.test_intentHash_crossLanguageVector`: pinned
/// in `contracts/test` against `GenericCallWrapper.intentHash` and in the SDK
/// against `genericIntentHash`, so the three encodings cannot drift apart
/// unnoticed.
#[test]
fn intent_hash_matches_the_solidity_vector() {
    let a = |n: u64| Address::left_padding_from(&n.to_be_bytes());
    let u = U256::from;
    let aux = |base: u64, q: u64, ct: &[u8]| IMasp::OutputAux {
        clueRx: u(base),
        clueRy: u(base + 1),
        clueQx: u(q),
        clueQy: u(q + 1),
        ephPubX: u(base + 2),
        ephPubY: u(base + 3),
        ciphertext: ct.to_vec().into(),
    };
    let b32 = |n: u64| FixedBytes::<32>::from(u(n).to_be_bytes::<32>());
    let deposit = |asset: u64, public_in: u64, inner: u64, fee_in: u64, fee_inner: u64| {
        IMasp::DepositRequest {
            chainId: u(31337u64),
            publicAssetId: asset,
            publicIn: public_in,
            payer: a(0x5A5A),
            recipient: a(0xBEEF),
            inner: b32(inner),
            feeAssetId: if fee_in == 0 { 0 } else { asset },
            feeIn: fee_in,
            feeInner: b32(fee_inner),
        }
    };
    // Only the hashed terms matter; the rest come from any valid payload.
    let args = IGenericCallWrapper::GenericArgs {
        refundTo: a(0x4EF0),
        surplusTo: a(0x5E55),
        deadline: u(1_900_000_000u64),
        minGas: u(600_000u64),
        calls: vec![
            IGenericCallWrapper::Call {
                target: a(0xCA11),
                value: U256::ZERO,
                data: vec![0xaa, 0xbb, 0xcc, 0xdd, 0x01].into(),
            },
            IGenericCallWrapper::Call {
                target: a(0xCA12),
                value: u(7u64),
                data: Vec::new().into(),
            },
        ],
        outputs: vec![
            IGenericCallWrapper::Output {
                minOut: u(9_900_000_000_000u64),
                deposit: deposit(2, 990, 1, 5, 6),
                aux: aux(10, 40, &[0x01, 0x02]),
                feeAux: aux(14, 42, &[0x03, 0x04, 0x05]),
            },
            IGenericCallWrapper::Output {
                minOut: u(30_000_000_000u64),
                deposit: deposit(3, 3, 0x21, 0, 0),
                aux: IMasp::OutputAux {
                    clueRx: u(50u64),
                    clueRy: u(51u64),
                    clueQx: u(52u64),
                    clueQy: u(53u64),
                    ephPubX: u(54u64),
                    ephPubY: u(55u64),
                    ciphertext: vec![0x09].into(),
                },
                feeAux: IMasp::OutputAux {
                    clueRx: u(56u64),
                    clueRy: u(57u64),
                    clueQx: u(58u64),
                    clueQy: u(59u64),
                    ephPubX: u(60u64),
                    ephPubY: u(61u64),
                    ciphertext: vec![0x0a, 0x0b].into(),
                },
            },
        ],
        refund_d: deposit(1, 995, 0x12, 22, 0x17),
        refund_aux_d: aux(27, 44, &[0x06]),
        refund_fee_aux_d: aux(31, 46, &[0x07, 0x08]),
        ..build_generic_args(&fake_payload()).unwrap()
    };
    assert_eq!(
        generic_intent_hash(&args).to_string(),
        "827219559487417732596895015167420095798095380869771450310630500175677464989"
    );
}

// --- the bundled call ---

fn item() -> GenericItem {
    let p = fake_payload();
    let (args, min_gas) = validate_generic(&p, &policy(&[(target(), SELECTOR)])).unwrap();
    GenericItem {
        nullifiers: p.pub_inputs.nullifier.to_vec(),
        batch: parse_spend_batch(&p.pub_inputs).unwrap(),
        merkle_root: merkle_root_of(&p.pub_inputs).unwrap(),
        wrapper: wrapper(),
        args,
        min_gas,
    }
}

/// The call the Bundler makes decodes back to the validated args, with the
/// batcher's tree-update proof and the reserved slot filled in.
#[test]
fn item_encodes_execute_with_the_reserved_tree_update() {
    let item = item();
    let slot = ReservedSlot {
        start_index: 12,
        old_root: [0u8; 32],
        old_frontier: Vec::new(),
        anchor_index: Some(3),
    };
    let advanced = AdvancedState {
        new_root: [9u8; 32],
    };
    let tp = IMasp::Proof {
        a: [U256::from(1u8), U256::from(2u8)],
        b: [
            [U256::from(3u8), U256::from(4u8)],
            [U256::from(5u8), U256::from(6u8)],
        ],
        c: [U256::from(7u8), U256::from(8u8)],
    };

    let call = item
        .encode(&slot, &advanced, U256::from(77u8), tp.clone())
        .unwrap();

    assert_eq!(call.target, wrapper());
    let decoded = IGenericCallWrapper::executeCall::abi_decode(&call.data, true)
        .unwrap()
        .a;
    assert_eq!(decoded.calls.len(), 2);
    assert_eq!(decoded.outputs.len(), 2);
    let expected = IGenericCallWrapper::GenericArgs {
        tp_w: tp,
        tpi_w: IMasp::SpendTree {
            newRoot: FixedBytes::from([9u8; 32]),
            startIndex: 12,
            anchorIndex: 3,
            digest: U256::from(77u8),
        },
        ..item.args.clone()
    };
    assert_eq!(decoded.abi_encode(), expected.abi_encode());
    // The tree update is outside the intent, so the encoded call still carries
    // the hash the wallet bound.
    assert_eq!(generic_intent_hash(&decoded), decoded.pi_w.intentHash);
}

#[test]
fn item_is_queued_as_generic_with_its_nullifiers() {
    let view = item().view();
    assert_eq!(view.kind, "generic");
    assert_eq!(view.nullifiers.len(), TRANSACT_IN);
    assert!(view.deposit_ids.is_empty());
}

// --- gas ---

/// Quoted, charged and weighed alike: the wrapper's overhead plus the whole
/// call-leg floor.
#[test]
fn gas_is_the_wrapper_overhead_plus_the_call_leg_floor() {
    let witness = GasWitness::new();
    let seed = witness.gas_for(EntryPoint::Generic);
    assert_eq!(generic_gas(&witness, 600_000), seed + 600_000);
    assert_eq!(item().gas_weight(&witness), seed + 600_000);

    witness.observe(EntryPoint::Generic, seed + 250_000);
    assert_eq!(generic_gas(&witness, 600_000), seed + 850_000);
    assert_eq!(generic_gas(&witness, u64::MAX), u64::MAX);
}
