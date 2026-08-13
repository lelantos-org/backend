//! Sparse-matrix schedule for the partial rounds.
//!
//! A partial round is `x -> M · S(x + c)`, where `S` raises only `x[0]` to the
//! fifth power. The full `M` costs `t²` multiplications, but almost all of that
//! work is redundant when the S-box touches one element.
//!
//! Factor `M = S̃ · P`, where
//!
//! ```text
//!     P  = [[1, 0ᵀ],           S̃ = [[m₀₀, bᵀ],
//!           [0, M̂ ]]                [v,   I ]]
//! ```
//!
//! with `M̂ = M[1.., 1..]`, `v = M[1.., 0]`, and `b` solving `M̂ᵀ b = M[0, 1..]`.
//! `P` acts as the identity on index 0, so it commutes with `S`, which lets it
//! be pushed back through the S-box into the previous round. Doing that for
//! every partial round leaves one `P` in front of the whole block and a `S̃` per
//! round costing `2t-1` multiplications instead of `t²` — for `t = 7`, 13
//! instead of 49, across the 57 partial rounds that dominate the permutation.
//!
//! This is an exact re-association, not an approximation: the output is
//! unchanged, which `super::tests` checks against `light-poseidon` directly.

use ark_ed_on_bn254::Fq;
use ark_ff::{Field, Zero};

/// `S̃`: dense first row, `v` down the first column, identity elsewhere.
struct Sparse {
    /// Row 0, length `t`.
    row0: Vec<Fq>,
    /// Column 0 below the diagonal, length `t - 1`.
    col: Vec<Fq>,
}

/// Everything the partial-round block needs.
///
/// The per-round matrices and constants are held in two flat buffers rather
/// than a `Vec` per round. There are `partial_rounds` of each — 63 at `t = 7` —
/// and the loop touches them strictly in order, so one contiguous walk beats
/// 126 separately-allocated vectors scattered across the heap. Flattening the
/// MDS the same way was most of what made the permutation faster to begin with.
pub(super) struct Schedule {
    /// `P` for the first partial round, applied once in front of the block.
    pub(super) pre: Vec<Fq>,
    /// `[row0 (t) | col (t - 1)]` per round.
    sparse: Vec<Fq>,
    /// Round constants pushed through the `P` matrices, `t` per round.
    /// `folded(0)` is added with the pre-multiplication, `folded(r)` after
    /// round `r - 1`.
    folded: Vec<Fq>,
    t: usize,
}

impl Schedule {
    /// Folded constants for round `r`.
    #[inline(always)]
    pub(super) fn folded(&self, r: usize) -> &[Fq] {
        &self.folded[r * self.t..(r + 1) * self.t]
    }

    /// `state <- S̃_r · state`.
    #[inline(always)]
    pub(super) fn apply_sparse(&self, r: usize, state: &mut [Fq]) {
        let stride = 2 * self.t - 1;
        let base = r * stride;
        let row0 = &self.sparse[base..base + self.t];
        let col = &self.sparse[base + self.t..base + stride];

        let x0 = state[0];
        // Row 0 needs the pre-update values, so accumulate before writing back.
        let acc = super::circom::dot(row0, state);
        for (s, m) in state[1..].iter_mut().zip(col) {
            *s += *m * x0;
        }
        state[0] = acc;
    }
}

impl Schedule {
    /// Derive the schedule from the dense MDS and the partial-round constants.
    ///
    /// `mds` is row-major `t × t`; `consts` is the round constant vector for
    /// each of the `partial_rounds` rounds, in order.
    pub(super) fn derive(mds: &[Fq], t: usize, consts: &[Vec<Fq>]) -> Option<Self> {
        let rounds = consts.len();
        debug_assert_eq!(mds.len(), t * t);

        let mut sparse = Vec::with_capacity(rounds);
        let mut primes = Vec::with_capacity(rounds);

        // Walk the block backwards: the last round decomposes the bare MDS,
        // and each `P` produced propagates into the round before it.
        let mut a = mds.to_vec();
        for _ in 0..rounds {
            let (s, p) = decompose(&a, t)?;
            sparse.push(s);
            a = mat_mul(&p, mds, t);
            primes.push(p);
        }
        sparse.reverse();
        primes.reverse();

        // `folded[r] = P_r · c_r`.
        let folded: Vec<Fq> = primes
            .iter()
            .zip(consts)
            .flat_map(|(p, c)| mat_vec(p, c, t))
            .collect();

        let flat: Vec<Fq> = sparse
            .into_iter()
            .flat_map(|s| s.row0.into_iter().chain(s.col))
            .collect();

        Some(Self {
            pre: primes.into_iter().next()?,
            sparse: flat,
            folded,
            t,
        })
    }
}

/// Split `a` into `(S̃, P)` with `a = S̃ · P`. `None` if `M̂` is singular, which
/// an MDS matrix never is.
fn decompose(a: &[Fq], t: usize) -> Option<(Sparse, Vec<Fq>)> {
    let n = t - 1;

    // M̂ = a[1.., 1..], transposed up front since that is what the solve needs.
    let mut hat_t = vec![Fq::ZERO; n * n];
    for i in 0..n {
        for j in 0..n {
            hat_t[j * n + i] = a[(i + 1) * t + (j + 1)];
        }
    }

    // b solves M̂ᵀ b = a[0, 1..].
    let w: Vec<Fq> = (1..t).map(|j| a[j]).collect();
    let b = solve(hat_t, w, n)?;

    let mut row0 = Vec::with_capacity(t);
    row0.push(a[0]);
    row0.extend_from_slice(&b);

    let col: Vec<Fq> = (1..t).map(|i| a[i * t]).collect();

    // P = [[1, 0], [0, M̂]]
    let mut p = vec![Fq::ZERO; t * t];
    p[0] = Fq::ONE;
    for i in 0..n {
        for j in 0..n {
            p[(i + 1) * t + (j + 1)] = a[(i + 1) * t + (j + 1)];
        }
    }

    Some((Sparse { row0, col }, p))
}

/// Gaussian elimination with partial pivoting. `m` is row-major `n × n` and is
/// consumed; returns `x` with `m · x = rhs`.
fn solve(mut m: Vec<Fq>, mut rhs: Vec<Fq>, n: usize) -> Option<Vec<Fq>> {
    for col in 0..n {
        let pivot = (col..n).find(|&r| !m[r * n + col].is_zero())?;
        if pivot != col {
            for j in 0..n {
                m.swap(col * n + j, pivot * n + j);
            }
            rhs.swap(col, pivot);
        }

        let inv = m[col * n + col].inverse()?;
        for j in col..n {
            m[col * n + j] *= inv;
        }
        rhs[col] *= inv;

        for r in 0..n {
            if r == col {
                continue;
            }
            let factor = m[r * n + col];
            if factor.is_zero() {
                continue;
            }
            for j in col..n {
                let v = m[col * n + j] * factor;
                m[r * n + j] -= v;
            }
            let v = rhs[col] * factor;
            rhs[r] -= v;
        }
    }
    Some(rhs)
}

/// Row-major `t × t` product `a · b`.
fn mat_mul(a: &[Fq], b: &[Fq], t: usize) -> Vec<Fq> {
    let mut out = vec![Fq::ZERO; t * t];
    for i in 0..t {
        for k in 0..t {
            let aik = a[i * t + k];
            if aik.is_zero() {
                continue;
            }
            for j in 0..t {
                out[i * t + j] += aik * b[k * t + j];
            }
        }
    }
    out
}

/// Row-major `t × t` times a length-`t` vector.
fn mat_vec(m: &[Fq], v: &[Fq], t: usize) -> Vec<Fq> {
    (0..t)
        .map(|i| {
            let mut acc = Fq::ZERO;
            for j in 0..t {
                acc += m[i * t + j] * v[j];
            }
            acc
        })
        .collect()
}
