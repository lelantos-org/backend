//! Parity against `light-poseidon`.
//!
//! `light-poseidon` is the oracle: the FMD test vectors were generated against
//! it. This crate's permutation may differ only in speed; a single differing bit
//! would change every note commitment and Merkle root and stop the circuits from
//! verifying.

use super::*;
use ark_ff::{Field, PrimeField, UniformRand};
use light_poseidon::{Poseidon, PoseidonBytesHasher, PoseidonHasher};
use rand::{RngCore, SeedableRng, rngs::StdRng};

/// Arities circomlib's `bn254_x5` tables cover: widths 2..=13.
const ARITIES: std::ops::RangeInclusive<usize> = 1..=12;

fn oracle(inputs: &[Fq]) -> Fq {
    Poseidon::<Fq>::new_circom(inputs.len())
        .expect("oracle construction")
        .hash(inputs)
        .expect("oracle hash")
}

#[test]
fn matches_light_poseidon_on_random_inputs() {
    let mut rng = StdRng::seed_from_u64(0x9051_D004);
    for arity in ARITIES {
        for case in 0..64 {
            let inputs: Vec<Fq> = (0..arity).map(|_| Fq::rand(&mut rng)).collect();
            assert_eq!(
                hash(&inputs).unwrap(),
                oracle(&inputs),
                "arity {arity}, case {case}"
            );
        }
    }
}

/// Edge values exercise the reduction paths random inputs rarely hit: zero, one,
/// and the largest element in the field.
#[test]
fn matches_light_poseidon_on_edge_values() {
    let edges = [Fq::ZERO, Fq::ONE, -Fq::ONE, Fq::from(2u64)];
    for arity in ARITIES {
        for edge in edges {
            let inputs = vec![edge; arity];
            assert_eq!(hash(&inputs).unwrap(), oracle(&inputs), "arity {arity}");
        }

        // Mixed, so element position matters.
        let mixed: Vec<Fq> = (0..arity)
            .map(|i| edges[i % edges.len()])
            .collect::<Vec<_>>();
        assert_eq!(hash(&mixed).unwrap(), oracle(&mixed), "arity {arity} mixed");
    }
}

/// Absolute anchor, as opposed to every other test in this file.
///
/// Parity with `light-poseidon` is relative: if that crate's constants changed
/// under a version bump, both sides would move together and every parity
/// assertion would still pass while every commitment and Merkle root changed.
/// These digests are fixed, so such a change fails a test.
///
/// The first two are the values circomlibjs publishes for `poseidon([1])` and
/// `poseidon([1, 2])`, which ties this implementation to circomlib itself
/// rather than to the crate we happen to source constants from. The rest are
/// regression locks recorded from this implementation once the published two
/// confirmed it.
#[test]
fn golden_digests_are_stable() {
    const GOLDEN: [(usize, &str); 5] = [
        // Published by circomlibjs.
        (
            1,
            "18586133768512220936620570745912940619677854269274689475585506675881198879027",
        ),
        (
            2,
            "7853200120776062878684798364095072458815029376092732009249414926327459813530",
        ),
        // Recorded from this implementation.
        (
            3,
            "6542985608222806190361240322586112750744169038454362455181422643027100751666",
        ),
        (
            4,
            "18821383157269793795438455681495246036402687001665670618754263018637548127333",
        ),
        (
            5,
            "6183221330272524995739186171720101788151706631170188140075976616310159254464",
        ),
    ];

    for (arity, expected) in GOLDEN {
        // Inputs are 1..=arity, matching how the published vectors are stated.
        let inputs: Vec<Fq> = (1..=arity as u64).map(Fq::from).collect();
        let got = hash(&inputs).unwrap();
        assert_eq!(
            got.into_bigint().to_string(),
            expected,
            "poseidon over 1..={arity}"
        );
    }
}

/// The filter hashes under rayon, so the thread-local hasher cache is built
/// concurrently in production. Each thread must end up with its own correct
/// instance.
#[test]
fn parallel_hashing_matches_serial() {
    use rayon::prelude::*;

    let mut rng = StdRng::seed_from_u64(0xBEEF);
    let cases: Vec<Vec<Fq>> = (0..512)
        .map(|i| {
            let arity = (i % 12) + 1;
            (0..arity).map(|_| Fq::rand(&mut rng)).collect()
        })
        .collect();

    let expected: Vec<Fq> = cases.iter().map(|c| oracle(c)).collect();
    let actual: Vec<Fq> = cases.par_iter().map(|c| hash(c).unwrap()).collect();

    assert_eq!(actual, expected);
}

#[test]
fn hash_bytes_be_matches_light_poseidon() {
    let mut rng = StdRng::seed_from_u64(0xBE_BE);
    for arity in ARITIES {
        for _ in 0..32 {
            // Top byte cleared so the value stays below the modulus.
            let raw: Vec<[u8; 32]> = (0..arity)
                .map(|_| {
                    let mut b = [0u8; 32];
                    rng.fill_bytes(&mut b);
                    b[0] = 0;
                    b
                })
                .collect();
            let refs: Vec<&[u8]> = raw.iter().map(|b| b.as_slice()).collect();

            let expected = Poseidon::<Fq>::new_circom(arity)
                .unwrap()
                .hash_bytes_be(&refs)
                .unwrap();
            assert_eq!(hash_bytes_be(&refs).unwrap(), expected, "arity {arity}");
        }
    }
}

/// Over-modulus input must be rejected rather than reduced, or two distinct byte
/// strings could hash to the same value.
#[test]
fn hash_bytes_be_rejects_non_canonical_input() {
    let too_big = [0xffu8; 32];
    assert!(matches!(
        hash_bytes_be(&[&too_big, &[1u8; 32]]),
        Err(PoseidonError::InputLargerThanModulus)
    ));
}

#[test]
fn wrong_input_count_is_rejected() {
    // `with_hasher` picks the hasher by input count, so the mismatch has to be
    // provoked directly against a fixed-arity instance.
    let h = circom::Circom::new(3).unwrap();
    assert!(matches!(
        h.hash(&[Fq::ONE, Fq::ONE]),
        Err(PoseidonError::WrongInputCount {
            got: 2,
            expected: 3
        })
    ));
}

#[test]
fn unsupported_arity_is_rejected() {
    assert!(matches!(
        hash(&vec![Fq::ONE; 13]),
        Err(PoseidonError::UnsupportedArity(13))
    ));
}

/// The cache must not leak state between calls or between arities.
#[test]
fn repeated_and_interleaved_calls_are_stable() {
    let mut rng = StdRng::seed_from_u64(3);
    let a: Vec<Fq> = (0..2).map(|_| Fq::rand(&mut rng)).collect();
    let b: Vec<Fq> = (0..5).map(|_| Fq::rand(&mut rng)).collect();
    let (ea, eb) = (oracle(&a), oracle(&b));

    for _ in 0..8 {
        assert_eq!(hash(&a).unwrap(), ea);
        assert_eq!(hash(&b).unwrap(), eb);
    }
}
