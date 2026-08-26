//! Fiat-Shamir challenge derivation for `tree_update_batch.circom`.
//!
//! Mirrors `contracts/src/libs/PubInputs.sol :: compress(TreeUpdateBatch)` so the
//! relayer feeds the prover the same `z` the contract derives from calldata.
//! Every array is leaf-indexed. Coefficient layout, `4 + 6 * MAX_L_BATCH`
//! entries:
//!
//! ```text
//! [0]                            oldRoot
//! [1]                            newRoot
//! [2]                            startIndex
//! [3]                            actualCount
//! [4 .. 3 + MAX_L]               cms[0 .. MAX_L-1]
//! [4 + MAX_L .. 3 + 3*MAX_L]     cvDeps flattened (x0, y0, x1, y1, …)
//! [4 + 3*MAX_L .. 3 + 4*MAX_L]   leafAsset[0 .. MAX_L-1]
//! [4 + 4*MAX_L .. 3 + 5*MAX_L]   leafPublicIn[0 .. MAX_L-1]
//! [4 + 5*MAX_L .. 3 + 6*MAX_L]   isDeposit[0 .. MAX_L-1]
//! ```

use crate::adapters::calldata::{MAX_L_BATCH, PaddedBatch};
use crate::adapters::parse::BN254_R;
use alloy::primitives::{U256, keccak256};
use alloy::sol_types::SolValue;
use fmd_crypto::tree::Field;

pub fn compute_z(
    old_root: &Field,
    new_root: &Field,
    start_index: u64,
    batch: &PaddedBatch,
) -> String {
    let mut coeffs: Vec<U256> = Vec::with_capacity(4 + 6 * MAX_L_BATCH);
    coeffs.push(U256::from_be_bytes(*old_root));
    coeffs.push(U256::from_be_bytes(*new_root));
    coeffs.push(U256::from(start_index));
    coeffs.push(U256::from(batch.actual_count));
    coeffs.extend(batch.cms.iter().map(|cm| U256::from_be_bytes(cm.0)));
    coeffs.extend(batch.cv_deps.iter().flatten().copied());
    coeffs.extend(batch.leaf_asset.iter().copied().map(U256::from));
    coeffs.extend(batch.leaf_public_in.iter().copied().map(U256::from));
    coeffs.extend(batch.is_deposit.iter().copied().map(U256::from));
    debug_assert_eq!(coeffs.len(), 4 + 6 * MAX_L_BATCH);

    let z = U256::from_be_bytes(keccak256(coeffs.abi_encode()).0) % *BN254_R;
    z.to_string()
}

#[cfg(test)]
mod tests {
    //! Golden `z` vectors copied from
    //! `circuits/vectors/tree-update-batch-4.json` (schema
    //! `lelantos.circuits.vectors/1`, template `TreeUpdateBatch(10, 4)`).
    //!
    //! `compute_z` mirrors `PubInputs.compress(TreeUpdateBatch)`, and the circuit
    //! Horner-evaluates the same coefficients at the same `z`. All three must
    //! agree, so pinning the published vectors catches a layout drift that would
    //! otherwise surface only on-chain: as `TreeUpdateRejected` on the flush path,
    //! and as `ProofRejected` on a spend, whose two proofs the batched verifier
    //! checks in one pairing and cannot attribute.
    //!
    //! Held as a table rather than one test per vector: the vectors are data
    //! published by another repo, so re-syncing them is a diff of rows rather than
    //! of code. `name` carries the identity into the failure message, and the
    //! array's declared length makes a row dropped in a re-sync a compile error
    //! rather than a shorter loop.

    use super::*;
    use alloy::primitives::FixedBytes;

    /// One published vector, in the JSON's own decimal-string form so a row can be
    /// copied across without reformatting.
    struct Vector {
        name: &'static str,
        old_root: &'static str,
        new_root: &'static str,
        start_index: u64,
        actual_count: u64,
        cms: [&'static str; MAX_L_BATCH],
        cv_deps: [[&'static str; 2]; MAX_L_BATCH],
        leaf_asset: [u64; MAX_L_BATCH],
        leaf_public_in: [u64; MAX_L_BATCH],
        is_deposit: [u8; MAX_L_BATCH],
        z: &'static str,
    }

    fn u256(dec: &str) -> U256 {
        U256::from_str_radix(dec, 10).expect("decimal field element")
    }

    fn field(dec: &str) -> Field {
        u256(dec).to_be_bytes()
    }

    impl Vector {
        /// The batch the relayer would have built for this vector.
        fn batch(&self) -> PaddedBatch {
            PaddedBatch {
                cms: self.cms.map(|d| FixedBytes::<32>::from(field(d))),
                cv_deps: self.cv_deps.map(|p| [u256(p[0]), u256(p[1])]),
                leaf_asset: self.leaf_asset,
                leaf_public_in: self.leaf_public_in,
                is_deposit: self.is_deposit,
                actual_count: self.actual_count,
            }
        }

        fn check(&self) {
            let z = compute_z(
                &field(self.old_root),
                &field(self.new_root),
                self.start_index,
                &self.batch(),
            );
            assert_eq!(z, self.z, "{}", self.name);
        }
    }

    const VECTORS: [Vector; 3] = [
        // One deposit leaf into an empty tree; per-leaf binding active.
        Vector {
            name: "single-deposit-empty-tree",
            old_root: "8609704094418396324511832574933371601208234217740666943293213721288143421607",
            new_root: "18982714174264210624719308725723541775850103495556838081460623913484912999053",
            start_index: 0,
            actual_count: 1,
            cms: [
                "1353326364211883747664361316770613763974263049855355126850897878619266451850",
                "0",
                "0",
                "0",
            ],
            cv_deps: [
                [
                    "14319940179928203678511201376905677924523897915598426125606239687477518800244",
                    "8047559278102977913200933481580879644121322180328429716633762344606343665242",
                ],
                ["0", "0"],
                ["0", "0"],
                ["0", "0"],
            ],
            leaf_asset: [7, 0, 0, 0],
            leaf_public_in: [1000, 0, 0, 0],
            is_deposit: [1, 0, 0, 0],
            z: "4939234609355588114356729475655222661844490388866714772471033493963803554156",
        },
        // Three leaves — the odd count a 3-output transact bundle produces.
        Vector {
            name: "odd-three-leaf-batch",
            old_root: "8609704094418396324511832574933371601208234217740666943293213721288143421607",
            new_root: "21111111995574014383501506628652439267483092886486391783740534242857993335558",
            start_index: 0,
            actual_count: 3,
            cms: [
                "1951742967319165803530964451547598624285840444203806646147021883492439581115",
                "19012133309391560335674557331482321887540911804271599306674289147830016117931",
                "21626009417826109968351082352763953063437352730508107439663320624509030242742",
                "0",
            ],
            cv_deps: [
                [
                    "13441643379034571655297438153911968311912066450823199808881616742788634694445",
                    "19857791822520468805879401469319430525694186493029692393325047521057511793125",
                ],
                [
                    "172184820202288636796606109394753434844894931468978803359234470296489320519",
                    "19929202248821796728182855623466572114254433864320370993374218539398216568798",
                ],
                [
                    "3232673491275830172697526198179687447807386226683463004658599168867972738030",
                    "21101527770759580632635866186811689111475176968401354171953005562139633329171",
                ],
                ["0", "0"],
            ],
            leaf_asset: [7, 0, 9, 0],
            leaf_public_in: [10, 0, 30, 0],
            is_deposit: [1, 0, 1, 0],
            z: "12166870973147045876668445569252918269814644531438373425846117441015825080760",
        },
        // Deposit and spend leaves in one batch at a non-zero start index.
        Vector {
            name: "mixed-batch-nonzero-start",
            old_root: "16317179763850847199255836009578461868788906640504234318695444798435183315946",
            new_root: "18791493954298985871190032798805294413512517775796641227739822589544247321992",
            start_index: 5,
            actual_count: 2,
            cms: [
                "10281311150437369658962254566811096262498888568221979102702155486735590083785",
                "3156928210729585595544303566872776843119585788599500213068457138534823449031",
                "0",
                "0",
            ],
            cv_deps: [
                [
                    "11420273972908799614402126845268278990431721906512074430938874970301015055718",
                    "10663703070210755634453872246035201458645451032689953160099224485747624683650",
                ],
                [
                    "1197526153745630025589416977751190632562661088387468230272852477653671502235",
                    "85499577431801573926957493556646934019004328713001728998230908293354264500",
                ],
                ["0", "0"],
                ["0", "0"],
            ],
            leaf_asset: [7, 0, 0, 0],
            leaf_public_in: [42, 0, 0, 0],
            is_deposit: [1, 0, 0, 0],
            z: "11390806752935300840939243126852028309282981710394294449703199603346036642258",
        },
    ];

    #[test]
    fn every_published_vector_matches_its_z() {
        for v in &VECTORS {
            v.check();
        }
    }
}
