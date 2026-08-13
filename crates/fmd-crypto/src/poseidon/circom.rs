//! Circom-compatible Poseidon permutation over BN254.
//!
//! Round constants and the MDS matrix come from `light-poseidon`'s
//! `bn254_x5` tables, so this is the same instance circomlib specifies — only
//! the evaluation is ours. `light-poseidon` re-parses those tables into a
//! fresh hasher per call, allocates a `Vec` for the state on every round, and
//! raises to the fifth power through the generic `Field::pow`. At one hash per
//! (note, subscription, bit) the filter feels all three.
//!
//! Equivalence with `light-poseidon` is asserted directly, over every
//! supported arity, in `super::tests`.

use super::PoseidonError;
use super::sparse::Schedule;
use ark_ed_on_bn254::Fq;
use ark_ff::Field;
use light_poseidon::parameters::bn254_x5::get_poseidon_parameters;

/// circomlib publishes `bn254_x5` constants up to width 13.
pub(super) const MAX_WIDTH: usize = 13;

/// `x^5`, the circomlib S-box.
///
/// Spelled out rather than `x.pow([5])`: the generic version walks the bits of
/// the exponent, which costs a loop and a branch per bit to reach the same
/// three multiplications.
#[inline(always)]
fn pow5(x: Fq) -> Fq {
    let x2 = x.square();
    let x4 = x2.square();
    x4 * x
}

/// Longest run of products `sum_of_products` can accumulate before it has to
/// reduce, for this field: `2 * (64 * N - modulus_bits) - 1`, which is
/// `2 * (256 - 254) - 1` for BN254.
const SOP_CHUNK: usize = 3;

/// Dot product with lazy Montgomery reduction.
///
/// `Fp::sum_of_products` interleaves the multiplications with the reduction, so
/// a run of terms costs one reduction rather than one per term. It takes fixed
/// -size arrays, and our width is only known at runtime, so the run length is
/// spelled out here. Every row of the MDS and the first row of each sparse
/// matrix is such a dot product.
///
/// Passing the whole row to `sum_of_products` and letting it chunk internally
/// was measured 4% slower than this explicit walk, so the chunking stays here.
#[inline(always)]
pub(super) fn dot(a: &[Fq], b: &[Fq]) -> Fq {
    let mut lhs = a.chunks_exact(SOP_CHUNK);
    let mut rhs = b.chunks_exact(SOP_CHUNK);

    let mut acc = Fq::ZERO;
    for (x, y) in lhs.by_ref().zip(rhs.by_ref()) {
        // Borrow rather than copy the chunk: `sum_of_products` wants `&[_; N]`,
        // and an owned array would memcpy `SOP_CHUNK` field elements per term.
        let x: &[Fq; SOP_CHUNK] = x.try_into().expect("chunks_exact yields SOP_CHUNK");
        let y: &[Fq; SOP_CHUNK] = y.try_into().expect("chunks_exact yields SOP_CHUNK");
        acc += Fq::sum_of_products(x, y);
    }
    for (x, y) in lhs.remainder().iter().zip(rhs.remainder()) {
        acc += *x * *y;
    }
    acc
}

pub struct Circom {
    width: usize,
    full_rounds: usize,
    partial_rounds: usize,
    /// Round constants, `width` per round, flattened.
    ark: Vec<Fq>,
    /// MDS matrix, row-major, `width * width`.
    mds: Vec<Fq>,
    /// Sparse-matrix rewrite of the partial-round block.
    schedule: Schedule,
}

impl Circom {
    /// Build the hasher for `arity` inputs (state width `arity + 1`).
    pub fn new(arity: usize) -> Result<Self, PoseidonError> {
        let width = arity
            .checked_add(1)
            .filter(|w| (2..=MAX_WIDTH).contains(w))
            .ok_or(PoseidonError::UnsupportedArity(arity))?;

        let p = get_poseidon_parameters::<Fq>(width as u8)
            .map_err(|e| PoseidonError::Params(e.to_string()))?;

        debug_assert_eq!(p.width, width);
        debug_assert_eq!(p.alpha, 5);
        debug_assert_eq!(p.ark.len(), width * (p.full_rounds + p.partial_rounds));

        let mds: Vec<Fq> = p.mds.into_iter().flatten().collect();

        // Constants belonging to the partial-round block, which the schedule
        // folds through its `P` matrices.
        let half = p.full_rounds / 2;
        let partial_consts: Vec<Vec<Fq>> = (half..half + p.partial_rounds)
            .map(|r| p.ark[r * width..(r + 1) * width].to_vec())
            .collect();

        let schedule = Schedule::derive(&mds, width, &partial_consts)
            .ok_or_else(|| PoseidonError::Params("MDS submatrix is singular".to_string()))?;

        Ok(Self {
            width,
            full_rounds: p.full_rounds,
            partial_rounds: p.partial_rounds,
            ark: p.ark,
            mds,
            schedule,
        })
    }

    /// Hash `inputs`, whose length must be `width - 1`.
    ///
    /// The state is seeded with the circom domain tag (zero) followed by the
    /// inputs, and the first state element is the digest.
    pub fn hash(&self, inputs: &[Fq]) -> Result<Fq, PoseidonError> {
        if inputs.len() + 1 != self.width {
            return Err(PoseidonError::WrongInputCount {
                got: inputs.len(),
                expected: self.width - 1,
            });
        }

        // Stack-allocated: `MAX_WIDTH` field elements is 416 bytes, and this
        // keeps the whole permutation free of heap traffic.
        let mut state = [Fq::ZERO; MAX_WIDTH];
        state[1..self.width].copy_from_slice(inputs);
        self.permute(&mut state);
        Ok(state[0])
    }

    fn permute(&self, state: &mut [Fq; MAX_WIDTH]) {
        let mut scratch = [Fq::ZERO; MAX_WIDTH];
        let half = self.full_rounds / 2;
        let mut round = 0;

        for _ in 0..half {
            self.add_round_constants(state, round);
            self.sbox_full(state);
            self.mix(state, &mut scratch);
            round += 1;
        }
        // Partial block, rewritten: one dense multiply by `P` in front, then a
        // sparse multiply per round. See `super::sparse`.
        let t = self.width;
        let sched = &self.schedule;
        for (i, (out, f)) in scratch[..t].iter_mut().zip(sched.folded(0)).enumerate() {
            *out = dot(&sched.pre[i * t..(i + 1) * t], &state[..t]) + f;
        }
        state[..t].copy_from_slice(&scratch[..t]);

        for r in 0..self.partial_rounds {
            state[0] = pow5(state[0]);
            sched.apply_sparse(r, &mut state[..t]);
            // The last round's successor constant belongs to the full round
            // that follows, which adds it the normal way.
            if r + 1 < self.partial_rounds {
                for (s, f) in state[..t].iter_mut().zip(sched.folded(r + 1)) {
                    *s += f;
                }
            }
        }
        round += self.partial_rounds;
        for _ in 0..half {
            self.add_round_constants(state, round);
            self.sbox_full(state);
            self.mix(state, &mut scratch);
            round += 1;
        }
    }

    #[inline(always)]
    fn add_round_constants(&self, state: &mut [Fq; MAX_WIDTH], round: usize) {
        let ark = &self.ark[round * self.width..(round + 1) * self.width];
        for (s, c) in state[..self.width].iter_mut().zip(ark) {
            *s += c;
        }
    }

    #[inline(always)]
    fn sbox_full(&self, state: &mut [Fq; MAX_WIDTH]) {
        for s in state[..self.width].iter_mut() {
            *s = pow5(*s);
        }
    }

    /// `state <- MDS * state`, through a scratch buffer so no allocation is
    /// needed to avoid aliasing.
    #[inline(always)]
    fn mix(&self, state: &mut [Fq; MAX_WIDTH], scratch: &mut [Fq; MAX_WIDTH]) {
        let t = self.width;
        for (i, out) in scratch[..t].iter_mut().enumerate() {
            *out = dot(&self.mds[i * t..(i + 1) * t], &state[..t]);
        }
        state[..t].copy_from_slice(&scratch[..t]);
    }
}
