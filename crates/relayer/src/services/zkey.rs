//! snarkjs zkey parser → arkworks `ProvingKey<Bn254>` + the A/B/C matrices.
//!
//! Vendored so the relayer does not depend on `ark-circom`, which pulls in
//! `wasmer` unconditionally to run the circom wasm witness generator this crate
//! replaced with a native witness graph. Ported from the browser prover's copy
//! at `sdk/wasm/prover/src/zkey.rs` and adapted to arkworks 0.6.
//!
//! Section layout:
//!   1  Header              (prover type)
//!   2  HeaderGroth         (n8q | q | n8r | r | nVars | nPub | domainSize | vk fields)
//!   3  IC                  (G1 × (nPub + 1))
//!   4  Coefficients        (matrices A, B)
//!   5  PointsA             (G1 × nVars)
//!   6  PointsB1            (G1 × nVars)
//!   7  PointsB2            (G2 × nVars)
//!   8  PointsC / L query   (G1 × (nVars - nPub - 1))
//!   9  PointsH             (G1 × domainSize)
//!  10  Contributions

use std::io::{Read, Seek, SeekFrom};

use ark_bn254::{Bn254, Fq, Fq2, Fr, G1Affine, G2Affine};
use ark_ff::PrimeField;
use ark_groth16::{ProvingKey, VerifyingKey};
use ark_relations::utils::matrix::Matrix;
use ark_serialize::{CanonicalDeserialize, SerializationError};
use byteorder::{LittleEndian, ReadBytesExt};
use num_traits::Zero;
use rayon::prelude::*;

const ZKEY_MAGIC: &[u8; 4] = b"zkey";
const G1_BYTES: usize = 64;
const G2_BYTES: usize = 128;
const FQ_BYTES: usize = 32;

type IoResult<T> = Result<T, SerializationError>;

// Section IDs are 1..=10 in a snarkjs groth16 zkey. Index 0 is unused.
const NUM_SECTION_SLOTS: usize = 11;
const SEC_HEADER_GROTH: u32 = 2;
const SEC_IC: u32 = 3;
const SEC_COEFFS: u32 = 4;
const SEC_A: u32 = 5;
const SEC_B1: u32 = 6;
const SEC_B2: u32 = 7;
const SEC_L: u32 = 8;
const SEC_H: u32 = 9;

/// The constraint matrices, and the shape metadata the prover passes alongside
/// them to `create_proof_with_reduction_and_matrices`.
pub struct ZkeyMatrices {
    /// The leading `1` plus the circuit's public outputs and inputs.
    pub num_instance_variables: usize,
    pub num_constraints: usize,
    pub a: Matrix<Fr>,
    pub b: Matrix<Fr>,
    /// Always empty: snarkjs does not store C, and the snarkjs QAP reduction
    /// ([`super::qap::CircomReduction`]) derives it from A and B. The prover
    /// still passes it, because the reduction indexes its argument by position.
    pub c: Matrix<Fr>,
}

/// Read a snarkjs zkey into an arkworks proving key and its matrices.
pub fn read_zkey<R: Read + Seek>(reader: &mut R) -> IoResult<(ProvingKey<Bn254>, ZkeyMatrices)> {
    let mut bin = BinFile::new(reader)?;
    let header = bin.groth_header()?;
    let pk = bin.proving_key(&header)?;
    let matrices = bin.matrices(&header)?;
    Ok((pk, matrices))
}

struct Section {
    position: u64,
}

struct BinFile<'a, R> {
    sections: [Option<Section>; NUM_SECTION_SLOTS],
    reader: &'a mut R,
}

impl<'a, R: Read + Seek> BinFile<'a, R> {
    fn new(reader: &'a mut R) -> IoResult<Self> {
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if &magic != ZKEY_MAGIC {
            return Err(malformed("not a .zkey file"));
        }
        let _version = reader.read_u32::<LittleEndian>()?;
        let num_sections = reader.read_u32::<LittleEndian>()?;

        let mut sections: [Option<Section>; NUM_SECTION_SLOTS] = Default::default();
        for _ in 0..num_sections {
            let id = reader.read_u32::<LittleEndian>()?;
            let len = reader.read_u64::<LittleEndian>()?;
            let position = reader.stream_position()?;
            // First occurrence wins; snarkjs writes each section once for
            // groth16. IDs outside the known 1..=10 range are skipped.
            if let Some(slot) = sections.get_mut(id as usize) {
                slot.get_or_insert(Section { position });
            }
            reader.seek(SeekFrom::Current(len as i64))?;
        }
        Ok(Self { sections, reader })
    }

    fn seek_to(&mut self, id: u32) -> IoResult<()> {
        let pos = self
            .sections
            .get(id as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| malformed(format!("zkey is missing section {id}")))?
            .position;
        self.reader.seek(SeekFrom::Start(pos))?;
        Ok(())
    }

    fn groth_header(&mut self) -> IoResult<HeaderGroth> {
        self.seek_to(SEC_HEADER_GROTH)?;
        HeaderGroth::read(&mut self.reader)
    }

    fn proving_key(&mut self, h: &HeaderGroth) -> IoResult<ProvingKey<Bn254>> {
        let ic = self.read_g1_section(SEC_IC, h.n_public + 1)?;
        let a_query = self.read_g1_section(SEC_A, h.n_vars)?;
        let b_g1_query = self.read_g1_section(SEC_B1, h.n_vars)?;
        let b_g2_query = self.read_g2_section(SEC_B2, h.n_vars)?;
        let l_query = self.read_g1_section(SEC_L, l_query_len(h.n_vars, h.n_public)?)?;
        let h_query = self.read_g1_section(SEC_H, h.domain_size as usize)?;

        let vk = VerifyingKey::<Bn254> {
            alpha_g1: h.vk.alpha_g1,
            beta_g2: h.vk.beta_g2,
            gamma_g2: h.vk.gamma_g2,
            delta_g2: h.vk.delta_g2,
            gamma_abc_g1: ic,
        };
        Ok(ProvingKey::<Bn254> {
            vk,
            beta_g1: h.vk.beta_g1,
            delta_g1: h.vk.delta_g1,
            a_query,
            b_g1_query,
            b_g2_query,
            h_query,
            l_query,
        })
    }

    fn matrices(&mut self, h: &HeaderGroth) -> IoResult<ZkeyMatrices> {
        self.seek_to(SEC_COEFFS)?;
        let num_coeffs = self.reader.read_u32::<LittleEndian>()?;

        // Rows grow on demand rather than preallocating `domain_size` empty Vec
        // slots: snarkjs domains reach 2^20 and most slots stay empty.
        let mut a_rows: Matrix<Fr> = Vec::new();
        let mut b_rows: Matrix<Fr> = Vec::new();
        let mut max_constraint = 0u32;

        for _ in 0..num_coeffs {
            let matrix = self.reader.read_u32::<LittleEndian>()?;
            let constraint = self.reader.read_u32::<LittleEndian>()?;
            let signal = self.reader.read_u32::<LittleEndian>()?;
            let value = read_fr(&mut self.reader)?;
            max_constraint = max_constraint.max(constraint);
            let rows = match matrix {
                0 => &mut a_rows,
                1 => &mut b_rows,
                _ => continue, // snarkjs only emits 0/1 for groth16
            };
            let idx = constraint as usize;
            if idx >= rows.len() {
                rows.resize_with(idx + 1, Vec::new);
            }
            rows[idx].push((value, signal as usize));
        }

        // Drop the trailing rows snarkjs adds for the public-input constraints;
        // arkworks re-adds them.
        let num_constraints = constraint_count(max_constraint as usize, h.n_public)?;
        a_rows.truncate(num_constraints);
        b_rows.truncate(num_constraints);
        a_rows.shrink_to_fit();
        b_rows.shrink_to_fit();

        Ok(ZkeyMatrices {
            num_instance_variables: h.n_public + 1,
            num_constraints,
            a: a_rows,
            b: b_rows,
            c: Vec::new(),
        })
    }

    fn read_g1_section(&mut self, id: u32, count: usize) -> IoResult<Vec<G1Affine>> {
        let buf = self.read_section(id, count, G1_BYTES)?;
        buf.par_chunks_exact(G1_BYTES).map(parse_g1).collect()
    }

    fn read_g2_section(&mut self, id: u32, count: usize) -> IoResult<Vec<G2Affine>> {
        let buf = self.read_section(id, count, G2_BYTES)?;
        buf.par_chunks_exact(G2_BYTES).map(parse_g2).collect()
    }

    /// Read `count` fixed-width points from section `id`.
    ///
    /// `count` comes from the header, so the buffer length is computed with a
    /// checked multiply: a corrupt `nVars` would otherwise wrap to a small
    /// allocation and silently parse the wrong bytes.
    fn read_section(&mut self, id: u32, count: usize, width: usize) -> IoResult<Vec<u8>> {
        let len = count
            .checked_mul(width)
            .ok_or_else(|| malformed(format!("zkey section {id} length overflows")))?;
        self.seek_to(id)?;
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// Length of the L query: every variable that is not a public input or the
/// leading constant `1`. A header claiming more public inputs than variables is
/// malformed, and the subtraction would otherwise wrap into a huge allocation.
fn l_query_len(n_vars: usize, n_public: usize) -> IoResult<usize> {
    n_vars
        .checked_sub(n_public + 1)
        .ok_or_else(|| malformed("zkey header has more public inputs than variables"))
}

/// Constraint count with the trailing public-input rows dropped, which arkworks
/// re-adds. Checked for the same reason as [`l_query_len`].
fn constraint_count(max_constraint: usize, n_public: usize) -> IoResult<usize> {
    max_constraint
        .checked_sub(n_public)
        .ok_or_else(|| malformed("zkey has fewer constraints than public inputs"))
}

/// A structurally invalid zkey. The header fields below are read straight out of
/// the file, so every value derived from them is checked rather than trusted:
/// unchecked arithmetic would underflow into a huge length and turn a malformed
/// file into an allocation abort instead of an error.
fn malformed(msg: impl Into<String>) -> SerializationError {
    SerializationError::IoError(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        msg.into(),
    ))
}

struct VkPoints {
    alpha_g1: G1Affine,
    beta_g1: G1Affine,
    beta_g2: G2Affine,
    gamma_g2: G2Affine,
    delta_g1: G1Affine,
    delta_g2: G2Affine,
}

impl VkPoints {
    fn read<R: Read>(reader: &mut R) -> IoResult<Self> {
        Ok(Self {
            alpha_g1: read_g1(reader)?,
            beta_g1: read_g1(reader)?,
            beta_g2: read_g2(reader)?,
            gamma_g2: read_g2(reader)?,
            delta_g1: read_g1(reader)?,
            delta_g2: read_g2(reader)?,
        })
    }
}

struct HeaderGroth {
    n_vars: usize,
    n_public: usize,
    domain_size: u32,
    vk: VkPoints,
}

impl HeaderGroth {
    fn read<R: Read + Seek>(mut reader: &mut R) -> IoResult<Self> {
        let n8q = u32::deserialize_uncompressed(&mut reader)?;
        reader.seek(SeekFrom::Current(n8q as i64))?; // q
        let n8r = u32::deserialize_uncompressed(&mut reader)?;
        reader.seek(SeekFrom::Current(n8r as i64))?; // r
        let n_vars = u32::deserialize_uncompressed(&mut reader)? as usize;
        let n_public = u32::deserialize_uncompressed(&mut reader)? as usize;
        let domain_size = u32::deserialize_uncompressed(&mut reader)?;
        let vk = VkPoints::read(&mut reader)?;
        Ok(Self {
            n_vars,
            n_public,
            domain_size,
            vk,
        })
    }
}

/// snarkjs writes Fr coefficients pre-multiplied by R^2. `new_unchecked` then
/// `into_bigint` divides by R once; wrapping again divides by R a second time,
/// landing in the standard (non-Montgomery) form arkworks consumers expect.
fn read_fr<R: Read>(reader: &mut R) -> IoResult<Fr> {
    let bigint = <Fr as PrimeField>::BigInt::deserialize_uncompressed(reader)?;
    Ok(Fr::new_unchecked(Fr::new_unchecked(bigint).into_bigint()))
}

fn read_g1<R: Read>(reader: &mut R) -> IoResult<G1Affine> {
    let mut buf = [0u8; G1_BYTES];
    reader.read_exact(&mut buf)?;
    parse_g1(&buf)
}

fn read_g2<R: Read>(reader: &mut R) -> IoResult<G2Affine> {
    let mut buf = [0u8; G2_BYTES];
    reader.read_exact(&mut buf)?;
    parse_g2(&buf)
}

fn parse_fq(bytes: &[u8]) -> IoResult<Fq> {
    let mut slice = bytes;
    let bigint = <Fq as PrimeField>::BigInt::deserialize_uncompressed(&mut slice)?;
    Ok(Fq::new_unchecked(bigint))
}

fn parse_fq2(bytes: &[u8]) -> IoResult<Fq2> {
    Ok(Fq2::new(
        parse_fq(&bytes[..FQ_BYTES])?,
        parse_fq(&bytes[FQ_BYTES..])?,
    ))
}

fn parse_g1(bytes: &[u8]) -> IoResult<G1Affine> {
    let x = parse_fq(&bytes[..FQ_BYTES])?;
    let y = parse_fq(&bytes[FQ_BYTES..])?;
    if x.is_zero() && y.is_zero() {
        Ok(G1Affine::identity())
    } else {
        // Trusted setup: the points are pre-validated, so this skips the
        // on-curve and subgroup checks.
        Ok(G1Affine::new_unchecked(x, y))
    }
}

fn parse_g2(bytes: &[u8]) -> IoResult<G2Affine> {
    let x = parse_fq2(&bytes[..2 * FQ_BYTES])?;
    let y = parse_fq2(&bytes[2 * FQ_BYTES..])?;
    if x.is_zero() && y.is_zero() {
        Ok(G2Affine::identity())
    } else {
        Ok(G2Affine::new_unchecked(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The L query covers the variables that are neither public nor the leading
    /// constant `1`.
    #[test]
    fn l_query_len_excludes_the_public_inputs_and_the_constant() {
        assert_eq!(l_query_len(10, 3).unwrap(), 6);
        assert_eq!(l_query_len(1, 0).unwrap(), 0);
    }

    /// A header with more public inputs than variables must be an error rather
    /// than a wrapped length that asks for a multi-exabyte allocation.
    #[test]
    fn l_query_len_rejects_more_public_inputs_than_variables() {
        assert!(l_query_len(2, 5).is_err());
        assert!(l_query_len(0, 0).is_err());
    }

    #[test]
    fn constraint_count_drops_the_public_input_rows() {
        assert_eq!(constraint_count(100, 2).unwrap(), 98);
    }

    #[test]
    fn constraint_count_rejects_fewer_constraints_than_public_inputs() {
        assert!(constraint_count(1, 5).is_err());
    }

    /// A file that is not a zkey must be refused by its magic, not parsed into
    /// nonsense section offsets.
    #[test]
    fn a_non_zkey_file_is_refused() {
        let mut cur = Cursor::new(b"not a zkey at all, just some bytes".to_vec());
        assert!(read_zkey(&mut cur).is_err());
    }

    /// Truncation must surface as an error rather than a panic: the section
    /// table is read from the file itself.
    #[test]
    fn a_truncated_zkey_is_an_error_not_a_panic() {
        // Valid magic, then a section table that runs off the end.
        let mut bytes = ZKEY_MAGIC.to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes()); // version
        bytes.extend_from_slice(&3u32.to_le_bytes()); // claims 3 sections
        bytes.extend_from_slice(&2u32.to_le_bytes()); // section id
        let mut cur = Cursor::new(bytes);
        assert!(read_zkey(&mut cur).is_err());
    }
}
