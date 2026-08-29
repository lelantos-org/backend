//! snarkjs-compatible R1CS-to-QAP reduction.
//!
//! Vendored from `ark-circom` 0.6 (MIT/Apache-2.0), which is otherwise pulled in
//! only for this and [`super::zkey`] — and which hard-depends on `wasmer` for the
//! circom wasm witness generator this crate no longer runs.
//!
//! The arkworks witness map computes the coefficients of H as (AB-C)/Z in the
//! evaluation domain and transforms back to the coefficient domain. snarkjs
//! instead precomputes the Lagrange form of the powers-of-tau bases in a domain
//! twice as large, and takes the witness map as the odd coefficients of (AB-C) in
//! that domain, serving as HZ when computing the C proof element. The zkey's
//! H query is laid out for the latter, so the two are not interchangeable.

use ark_ff::PrimeField;
use ark_groth16::r1cs_to_qap::{LibsnarkReduction, R1CSToQAP, evaluate_constraint};
use ark_poly::EvaluationDomain;
use ark_relations::gr1cs::{ConstraintSystemRef, SynthesisError};
use ark_relations::utils::matrix::Matrix;
use rayon::prelude::*;

pub struct CircomReduction;

impl R1CSToQAP for CircomReduction {
    #[allow(clippy::type_complexity)]
    fn instance_map_with_evaluation<F: PrimeField, D: EvaluationDomain<F>>(
        cs: ConstraintSystemRef<F>,
        t: &F,
    ) -> Result<(Vec<F>, Vec<F>, Vec<F>, F, usize, usize), SynthesisError> {
        LibsnarkReduction::instance_map_with_evaluation::<F, D>(cs, t)
    }

    fn witness_map_from_matrices<F: PrimeField, D: EvaluationDomain<F>>(
        matrices: &[Matrix<F>],
        num_inputs: usize,
        num_constraints: usize,
        full_assignment: &[F],
    ) -> Result<Vec<F>, SynthesisError> {
        let zero = F::zero();
        let domain =
            D::new(num_constraints + num_inputs).ok_or(SynthesisError::PolynomialDegreeTooLarge)?;
        let domain_size = domain.size();

        let mut a = vec![zero; domain_size];
        let mut b = vec![zero; domain_size];

        a[..num_constraints]
            .par_iter_mut()
            .zip(&mut (b[..num_constraints]))
            .zip(&matrices[0])
            .zip(&matrices[1])
            .for_each(|(((a, b), at_i), bt_i)| {
                *a = evaluate_constraint(at_i, full_assignment);
                *b = evaluate_constraint(bt_i, full_assignment);
            });

        {
            let start = num_constraints;
            let end = start + num_inputs;
            a[start..end].clone_from_slice(&full_assignment[..num_inputs]);
        }

        let mut c = vec![zero; domain_size];
        c[..num_constraints]
            .par_iter_mut()
            .zip(&a)
            .zip(&b)
            .for_each(|((c_i, a), b)| {
                *c_i = *a * b;
            });

        domain.ifft_in_place(&mut a);
        domain.ifft_in_place(&mut b);

        let root_of_unity = {
            let domain_size_double = 2 * domain_size;
            let domain_double =
                D::new(domain_size_double).ok_or(SynthesisError::PolynomialDegreeTooLarge)?;
            domain_double.element(1)
        };
        D::distribute_powers_and_mul_by_const(&mut a, root_of_unity, F::one());
        D::distribute_powers_and_mul_by_const(&mut b, root_of_unity, F::one());

        domain.fft_in_place(&mut a);
        domain.fft_in_place(&mut b);

        let mut ab = domain.mul_polynomials_in_evaluation_domain(&a, &b);
        drop(a);
        drop(b);

        domain.ifft_in_place(&mut c);
        D::distribute_powers_and_mul_by_const(&mut c, root_of_unity, F::one());
        domain.fft_in_place(&mut c);

        ab.par_iter_mut()
            .zip(c)
            .for_each(|(ab_i, c_i)| *ab_i -= &c_i);

        Ok(ab)
    }

    fn h_query_scalars<F: PrimeField, D: EvaluationDomain<F>>(
        max_power: usize,
        t: F,
        _: F,
        delta_inverse: F,
    ) -> Result<Vec<F>, SynthesisError> {
        // The usual H query has domain-1 powers and Z has domain powers, so HZ
        // has 2*domain-1.
        let mut scalars = (0..2 * max_power + 1)
            .into_par_iter()
            .map(|i| delta_inverse * t.pow([i as u64]))
            .collect::<Vec<_>>();
        let domain_size = scalars.len();
        let domain = D::new(domain_size).ok_or(SynthesisError::PolynomialDegreeTooLarge)?;
        domain.ifft_in_place(&mut scalars);
        Ok(scalars.into_par_iter().skip(1).step_by(2).collect())
    }
}
