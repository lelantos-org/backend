use ark_ed_on_bn254::Fr;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use fmd_crypto::clue::{base8_circom, scalar_mul, test_clue, test_clue_batch};
use rand::{Rng, SeedableRng, rngs::StdRng};

const GAMMA: usize = 8;

fn gen_inputs(k: usize, seed: u64) -> (Vec<Vec<Fr>>, fmd_crypto::clue::CircomPoint, u16) {
    let mut rng = StdRng::seed_from_u64(seed);
    let dks: Vec<Vec<Fr>> = (0..k)
        .map(|_| (0..GAMMA).map(|_| Fr::from(rng.r#gen::<u64>())).collect())
        .collect();
    // Build a real clue point R = r * G so it lies on the curve.
    let r_scalar = Fr::from(rng.r#gen::<u64>());
    let r = scalar_mul(base8_circom(), r_scalar);
    let bits: u16 = rng.r#gen::<u16>() & ((1u16 << GAMMA) - 1);
    (dks, r, bits)
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("clue_filter");
    for k in [1_000usize, 10_000usize] {
        let (dks, r, bits) = gen_inputs(k, 0xC0FFEE ^ (k as u64));
        let dk_refs: Vec<&[Fr]> = dks.iter().map(|d| d.as_slice()).collect();
        group.throughput(Throughput::Elements(k as u64));

        group.bench_with_input(BenchmarkId::new("scalar_loop", k), &k, |b, _| {
            b.iter(|| {
                let mut hits = 0usize;
                for dk in &dks {
                    if test_clue(dk, r, bits, GAMMA) {
                        hits += 1;
                    }
                }
                criterion::black_box(hits)
            });
        });

        group.bench_with_input(BenchmarkId::new("batch", k), &k, |b, _| {
            b.iter(|| {
                let res = test_clue_batch(&dk_refs, r, bits, GAMMA);
                criterion::black_box(res)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
