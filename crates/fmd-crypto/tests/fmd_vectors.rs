use ark_ed_on_bn254::Fr;
use fmd_crypto::clue::{
    base8_circom, fq_from_dec, fr_from_dec, pack, scalar_mul, test_clue, test_clue_batch, unpack,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct VectorFile {
    vectors: Vec<Vector>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct Vector {
    label: String,
    gamma: u8,
    dk_x: Vec<String>,
    fk_X: Vec<Point>,
    r: String,
    clue_R: String,
    clue_bits: String,
    clue_encoded: String,
    detect_self: bool,
    detect_other: bool,
}

#[derive(Debug, Deserialize)]
struct Point {
    x: String,
    y: String,
}

fn load() -> VectorFile {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/fmd.json"
    ))
    .expect("vectors");
    serde_json::from_str(&text).expect("parse")
}

fn h2b(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).expect("hex")
}

#[test]
fn base8_on_curve() {
    assert!(base8_circom().is_on_curve());
}

#[test]
fn dk_to_fk_consistency() {
    let g = base8_circom();
    for v in load().vectors {
        for (i, sk_dec) in v.dk_x.iter().enumerate() {
            let sk = fr_from_dec(sk_dec);
            let computed = scalar_mul(g, sk);
            assert_eq!(
                computed.x,
                fq_from_dec(&v.fk_X[i].x),
                "{}: fk_X[{}].x",
                v.label,
                i
            );
            assert_eq!(
                computed.y,
                fq_from_dec(&v.fk_X[i].y),
                "{}: fk_X[{}].y",
                v.label,
                i
            );
        }
    }
}

#[test]
fn r_packed_roundtrip() {
    for v in load().vectors {
        let r_bytes = h2b(&v.clue_R);
        let p = unpack(&r_bytes).unwrap_or_else(|e| panic!("{}: unpack {:?}", v.label, e));
        assert!(p.is_on_curve(), "{}: R off curve", v.label);
        assert_eq!(pack(&p).to_vec(), r_bytes, "{}: roundtrip", v.label);
    }
}

#[test]
fn r_equals_r_times_base() {
    let g = base8_circom();
    for v in load().vectors {
        let r_scalar = fr_from_dec(&v.r);
        let computed = scalar_mul(g, r_scalar);
        let r_unpacked = unpack(&h2b(&v.clue_R)).unwrap();
        assert_eq!(computed, r_unpacked, "{}", v.label);
    }
}

#[test]
fn clue_encoded_layout() {
    for v in load().vectors {
        let encoded = h2b(&v.clue_encoded);
        let bits = h2b(&v.clue_bits);
        let r_compressed = h2b(&v.clue_R);
        assert_eq!(encoded[0], v.gamma, "{}: gamma byte", v.label);
        assert_eq!(
            &encoded[1..33],
            r_compressed.as_slice(),
            "{}: R bytes",
            v.label
        );
        assert_eq!(&encoded[33..], bits.as_slice(), "{}: bits bytes", v.label);
    }
}

#[test]
fn detect_self_match() {
    let mut failures = Vec::new();
    for v in load().vectors {
        let dk: Vec<_> = v.dk_x.iter().map(|s| fr_from_dec(s)).collect();
        let r = unpack(&h2b(&v.clue_R)).unwrap();
        // Vectors store clue_bits as a single byte (gamma ≤ 8 cases) or two bytes
        // for gamma=16. Tests cover gamma ∈ {3, 5, 8} → 1 byte each.
        let bits_bytes = h2b(&v.clue_bits);
        let bits: u16 = match bits_bytes.len() {
            1 => bits_bytes[0] as u16,
            2 => u16::from_le_bytes([bits_bytes[0], bits_bytes[1]]),
            _ => panic!("unexpected clue_bits length"),
        };
        let gamma = v.gamma as usize;
        let result = test_clue(&dk, r, bits, gamma);
        if result != v.detect_self {
            failures.push(format!(
                "{}: got {}, want {}",
                v.label, result, v.detect_self
            ));
        }
        let _ = v.detect_other;
    }
    assert!(failures.is_empty(), "test_clue:\n{}", failures.join("\n"));
}

#[test]
fn batch_matches_scalar() {
    // Batch of the real dk plus 7 perturbed dks must equal per-key test_clue.
    for v in load().vectors {
        let real_dk: Vec<Fr> = v.dk_x.iter().map(|s| fr_from_dec(s)).collect();
        let r = unpack(&h2b(&v.clue_R)).unwrap();
        let bits_bytes = h2b(&v.clue_bits);
        let bits: u16 = match bits_bytes.len() {
            1 => bits_bytes[0] as u16,
            2 => u16::from_le_bytes([bits_bytes[0], bits_bytes[1]]),
            _ => panic!("unexpected clue_bits length"),
        };
        let gamma = v.gamma as usize;

        let mut all_dks: Vec<Vec<Fr>> = Vec::new();
        all_dks.push(real_dk.clone());
        for seed in 0..7u64 {
            let mut perturbed = real_dk.clone();
            let idx = (seed as usize) % gamma;
            perturbed[idx] += Fr::from(seed + 1);
            all_dks.push(perturbed);
        }

        let dk_refs: Vec<&[Fr]> = all_dks.iter().map(|d| d.as_slice()).collect();
        let batch = test_clue_batch(&dk_refs, r, bits, gamma);
        let scalar: Vec<bool> = all_dks
            .iter()
            .map(|dk| test_clue(dk, r, bits, gamma))
            .collect();
        assert_eq!(batch, scalar, "{}: batch vs scalar mismatch", v.label);
    }
}

#[test]
fn batch_empty_and_edges() {
    let v = load()
        .vectors
        .into_iter()
        .next()
        .expect("at least one vector");
    let real_dk: Vec<Fr> = v.dk_x.iter().map(|s| fr_from_dec(s)).collect();
    let r = unpack(&h2b(&v.clue_R)).unwrap();
    let bits_bytes = h2b(&v.clue_bits);
    let bits: u16 = match bits_bytes.len() {
        1 => bits_bytes[0] as u16,
        2 => u16::from_le_bytes([bits_bytes[0], bits_bytes[1]]),
        _ => panic!(),
    };
    let gamma = v.gamma as usize;

    assert_eq!(test_clue_batch(&[], r, bits, gamma), Vec::<bool>::new());

    let one = vec![real_dk.as_slice()];
    assert_eq!(test_clue_batch(&one, r, bits, gamma), vec![v.detect_self]);

    let short = vec![Fr::from(0u64)];
    let bad = vec![short.as_slice(), real_dk.as_slice()];
    let res = test_clue_batch(&bad, r, bits, gamma);
    assert_eq!(res.len(), 2);
    assert!(!res[0], "wrong-length dk must fail");
    assert_eq!(res[1], v.detect_self);
}
