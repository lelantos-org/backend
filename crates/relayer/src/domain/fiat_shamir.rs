// Fiat-Shamir challenge derivation for `tree_update_batch.circom`.
// Mirrors `contracts/src/libs/PubInputs.sol :: compress(TreeUpdateBatch)`
// so the relayer feeds the same `z` to the prover that the contract derives
// from calldata. Every array is leaf-indexed; coefficient layout
// (4 + 6*MAX_L_BATCH = 28 entries):
//   [0]                                  oldRoot
//   [1]                                  newRoot
//   [2]                                  startIndex
//   [3]                                  actualCount
//   [4 .. 3 + MAX_L]                     cms[0 .. MAX_L-1]
//   [4 + MAX_L .. 3 + 3*MAX_L]           cvDeps flat (x0,y0,x1,y1,...)
//   [4 + 3*MAX_L .. 3 + 4*MAX_L]         leafAsset[0..MAX_L-1]
//   [4 + 4*MAX_L .. 3 + 5*MAX_L]         leafPublicIn[0..MAX_L-1]
//   [4 + 5*MAX_L .. 3 + 6*MAX_L]         isDeposit[0..MAX_L-1]

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
    //! `compute_z` mirrors `PubInputs.compress(TreeUpdateBatch)`, and the
    //! circuit Horner-evaluates the same 28 coefficients at the same `z`. All
    //! three must agree, so pinning the published vectors here catches a
    //! layout drift that would otherwise surface only on-chain — as
    //! `TreeUpdateRejected` on the flush path, and as `ProofRejected` on a
    //! spend, whose two proofs the batched verifier checks in one pairing and
    //! cannot attribute.

    use super::*;
    use alloy::primitives::FixedBytes;

    fn field(dec: &str) -> Field {
        U256::from_str_radix(dec, 10)
            .expect("decimal field element")
            .to_be_bytes()
    }

    fn cms(decs: [&str; MAX_L_BATCH]) -> [FixedBytes<32>; MAX_L_BATCH] {
        decs.map(|d| FixedBytes::<32>::from(field(d)))
    }

    fn cvs(decs: [[&str; 2]; MAX_L_BATCH]) -> [[U256; 2]; MAX_L_BATCH] {
        decs.map(|p| {
            [
                U256::from_str_radix(p[0], 10).expect("cv x"),
                U256::from_str_radix(p[1], 10).expect("cv y"),
            ]
        })
    }

    /// A vector's published arrays, as the batch the relayer would have built.
    fn batch(
        cms: [FixedBytes<32>; MAX_L_BATCH],
        cv_deps: [[U256; 2]; MAX_L_BATCH],
        leaf_asset: [u64; MAX_L_BATCH],
        leaf_public_in: [u64; MAX_L_BATCH],
        is_deposit: [u8; MAX_L_BATCH],
        actual_count: u64,
    ) -> PaddedBatch {
        PaddedBatch {
            cms,
            cv_deps,
            leaf_asset,
            leaf_public_in,
            is_deposit,
            actual_count,
        }
    }

    /// `single-deposit-empty-tree` — One deposit leaf into an empty tree; per-leaf binding active.
    #[test]
    fn single_deposit_empty_tree_matches_the_published_z() {
        let cms = cms([
            "1353326364211883747664361316770613763974263049855355126850897878619266451850",
            "0",
            "0",
            "0",
        ]);
        let cv_deps = cvs([
            [
                "14319940179928203678511201376905677924523897915598426125606239687477518800244",
                "8047559278102977913200933481580879644121322180328429716633762344606343665242",
            ],
            ["0", "0"],
            ["0", "0"],
            ["0", "0"],
        ]);
        let leaf_asset: [u64; MAX_L_BATCH] = [7, 0, 0, 0];
        let leaf_public_in: [u64; MAX_L_BATCH] = [1000, 0, 0, 0];
        let is_deposit: [u8; MAX_L_BATCH] = [1, 0, 0, 0];

        let z = compute_z(
            &field("8609704094418396324511832574933371601208234217740666943293213721288143421607"),
            &field("18982714174264210624719308725723541775850103495556838081460623913484912999053"),
            0,
            &batch(cms, cv_deps, leaf_asset, leaf_public_in, is_deposit, 1),
        );
        assert_eq!(
            z,
            "4939234609355588114356729475655222661844490388866714772471033493963803554156"
        );
    }

    /// `odd-three-leaf-batch` — Three leaves — the odd count a 3-output transact bundle produces.
    #[test]
    fn odd_three_leaf_batch_matches_the_published_z() {
        let cms = cms([
            "1951742967319165803530964451547598624285840444203806646147021883492439581115",
            "19012133309391560335674557331482321887540911804271599306674289147830016117931",
            "21626009417826109968351082352763953063437352730508107439663320624509030242742",
            "0",
        ]);
        let cv_deps = cvs([
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
        ]);
        let leaf_asset: [u64; MAX_L_BATCH] = [7, 0, 9, 0];
        let leaf_public_in: [u64; MAX_L_BATCH] = [10, 0, 30, 0];
        let is_deposit: [u8; MAX_L_BATCH] = [1, 0, 1, 0];

        let z = compute_z(
            &field("8609704094418396324511832574933371601208234217740666943293213721288143421607"),
            &field("21111111995574014383501506628652439267483092886486391783740534242857993335558"),
            0,
            &batch(cms, cv_deps, leaf_asset, leaf_public_in, is_deposit, 3),
        );
        assert_eq!(
            z,
            "12166870973147045876668445569252918269814644531438373425846117441015825080760"
        );
    }

    /// `mixed-batch-nonzero-start` — Deposit and spend leaves in one batch at a non-zero start index.
    #[test]
    fn mixed_batch_nonzero_start_matches_the_published_z() {
        let cms = cms([
            "10281311150437369658962254566811096262498888568221979102702155486735590083785",
            "3156928210729585595544303566872776843119585788599500213068457138534823449031",
            "0",
            "0",
        ]);
        let cv_deps = cvs([
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
        ]);
        let leaf_asset: [u64; MAX_L_BATCH] = [7, 0, 0, 0];
        let leaf_public_in: [u64; MAX_L_BATCH] = [42, 0, 0, 0];
        let is_deposit: [u8; MAX_L_BATCH] = [1, 0, 0, 0];

        let z = compute_z(
            &field("16317179763850847199255836009578461868788906640504234318695444798435183315946"),
            &field("18791493954298985871190032798805294413512517775796641227739822589544247321992"),
            5,
            &batch(cms, cv_deps, leaf_asset, leaf_public_in, is_deposit, 2),
        );
        assert_eq!(
            z,
            "11390806752935300840939243126852028309282981710394294449703199603346036642258"
        );
    }
}
