//! Cost breakdown of one FMD bit test.
//!
//! `test_clue_batch` spends its time in three places per surviving key and bit:
//! a Baby-Jubjub scalar multiplication, a Poseidon-6 hash, and the Legendre
//! symbol that turns the hash into a bit. This bench prices each separately so a
//! change to the filter loop can target whichever dominates.

use ark_ed_on_bn254::{Fq, Fr};
use ark_ff::{Field, UniformRand};
use criterion::{Criterion, criterion_group, criterion_main};
use fmd_crypto::clue::{FixedBaseTable, base8_circom, scalar_mul};
use fmd_crypto::poseidon::hash as poseidon_hash;
use rand::{SeedableRng, rngs::StdRng};

const KEYS: usize = 1_000;

fn bench(c: &mut Criterion) {
    let mut rng = StdRng::seed_from_u64(0xC0FFEE);
    let r = scalar_mul(base8_circom(), Fr::rand(&mut rng));
    let scalars: Vec<Fr> = (0..KEYS).map(|_| Fr::rand(&mut rng)).collect();
    let field = Fq::rand(&mut rng);
    let inputs = [field; 6];

    let mut group = c.benchmark_group("bit_test");

    // Table construction is per (note, gamma) rather than per key, so it matters
    // only if it is large relative to the multiplications it amortises.
    group.bench_function("fixed_base_table_new", |b| {
        b.iter(|| criterion::black_box(FixedBaseTable::new(r, KEYS)));
    });

    let table = FixedBaseTable::new(r, KEYS);
    group.bench_function("batch_mul_1000", |b| {
        b.iter(|| criterion::black_box(table.batch_mul(&scalars)));
    });

    group.bench_function("scalar_mul_single", |b| {
        b.iter(|| criterion::black_box(scalar_mul(r, scalars[0])));
    });

    group.bench_function("poseidon6", |b| {
        b.iter(|| criterion::black_box(poseidon_hash(&inputs).unwrap()));
    });

    group.bench_function("legendre", |b| {
        b.iter(|| criterion::black_box(field.legendre()));
    });

    group.finish();
}

criterion_group!(benches, bench, bench_tree, bench_construction);
criterion_main!(benches);

/// Tree costs on the two significant paths: the bulk fill a mirror performs at
/// bootstrap, and the single insert a submission performs while holding the
/// per-chain lock.
///
/// `root()` is not benchmarked: the tree keeps internal nodes materialised, so
/// the hashing appears in `insert` and `extend`.
fn bench_tree(c: &mut Criterion) {
    use fmd_crypto::tree::MerkleTree;

    fn leaf_at(i: usize) -> [u8; 32] {
        let mut leaf = [0u8; 32];
        leaf[24..].copy_from_slice(&(i as u64).to_be_bytes());
        leaf
    }

    let mut group = c.benchmark_group("tree");
    group.sample_size(10);

    for leaves in [1_000usize, 10_000] {
        let field = Fq::rand(&mut StdRng::seed_from_u64(3));
        let leaf_inputs = [field; 4];

        group.bench_function(format!("leaf_hash_x{leaves}"), |b| {
            b.iter(|| {
                for _ in 0..leaves {
                    criterion::black_box(poseidon_hash(&leaf_inputs).unwrap());
                }
            });
        });

        // Bootstrap: bulk fill, hashed level by level across rayon threads.
        group.bench_function(format!("extend_{leaves}"), |b| {
            b.iter(|| {
                let mut tree = MerkleTree::new(10).unwrap();
                tree.extend((0..leaves).map(leaf_at)).unwrap();
                criterion::black_box(tree.root().unwrap())
            });
        });

        // Steady state: one leaf appended to a tree of this size. Expected to be
        // flat in `leaves`, since it re-hashes one root path rather than the
        // tree.
        let mut tree = MerkleTree::new(10).unwrap();
        tree.extend((0..leaves).map(leaf_at)).unwrap();
        group.bench_function(format!("insert_into_{leaves}"), |b| {
            b.iter(|| {
                tree.insert(leaf_at(leaves)).unwrap();
                tree.truncate_leaves(1).unwrap();
                criterion::black_box(tree.root().unwrap())
            });
        });
    }
    group.finish();
}

/// Construction cost. Deriving the sparse matrices runs a Gaussian solve per
/// partial round, paid once per arity per thread, so it pays off only when
/// amortised over many hashes.
fn bench_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("construction");
    group.sample_size(20);

    group.bench_function("ours_arity6", |b| {
        b.iter(|| {
            // First call on a fresh thread pays the schedule derivation.
            criterion::black_box(
                std::thread::spawn(|| {
                    let inputs = [Fq::from(1u64); 6];
                    poseidon_hash(&inputs).unwrap()
                })
                .join()
                .unwrap(),
            )
        });
    });

    group.bench_function("light_poseidon_arity6", |b| {
        use light_poseidon::{Poseidon, PoseidonHasher};
        b.iter(|| {
            let mut p = Poseidon::<Fq>::new_circom(6).unwrap();
            criterion::black_box(p.hash(&[Fq::from(1u64); 6]).unwrap())
        });
    });

    group.finish();
}
