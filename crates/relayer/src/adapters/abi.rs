#![allow(clippy::too_many_arguments)]
use alloy::sol;

sol! {
    #![sol(rpc)]
    /// MASP, SwapWrapper and NativeAdapter ABIs. The field layout must match
    /// `contracts/src/MASP.sol`, `contracts/src/libs/PubInputs.sol`,
    /// `contracts/src/swap/*.sol` and
    /// `contracts/src/native/NativeAdapter.sol`. Declared in one `sol!`
    /// invocation so the wrapper and the adapter can reference
    /// `IMasp.{Proof, Transact, SpendTree, OutputAux, DepositRequest}`
    /// without duplicating types.
    interface IMasp {
        struct Proof {
            uint256[2] a;
            uint256[2][2] b;
            uint256[2] c;
        }
        /// `PubInputs.Transact`. The array arity is the deployed circuit's
        /// `TRANSACT_IN` (4) and `TRANSACT_OUT` (6). `sol!` takes literals, so
        /// the arity cannot be written in terms of the Rust constants;
        /// `services::transact_verifier::public_signals` asserts the pair at compile time. Changing the
        /// arity requires a new circuit, ceremony and verifier.
        struct Transact {
            bytes32 merkleRoot;
            bytes32[4] nullifier;
            bytes32[6] outCm;
            uint64 publicAssetId;
            uint64 publicIn;
            uint64 publicOut;
            uint256[2][4] inCv;
            uint256[2][6] outCv;
            uint256[2][6] outCvDep;
            /// Challenge-only: hashed into `z`, never evaluated into `y`. They
            /// follow every pinned member, as in `PubInputs.Transact`, so the
            /// coefficients are the calldata block's leading words.
            address recipient;
            uint256 chainId;
            address payer;
            address relayer;
            /// `SwapWrapper._intentHash`: binds a swap's output
            /// (`deposit_d`, aux payloads), `minOut`, adapter, deadline and
            /// `refundTo` to the proof. Only the wrapper reads it (and reverts
            /// `IntentMismatch`); the pool and `NativeAdapter` ignore it.
            uint256 intentHash;
        }
        /// `PubInputs.TreeUpdateBatch`. Every array is indexed by leaf rather
        /// than by pair: `actualCount` is a leaf count in `[1, MAX_L_BATCH]`, so
        /// a batch may commit an odd number of leaves. Slots beyond
        /// `actualCount` must be zero, both in-circuit and on-chain.
        struct TreeUpdateBatch {
            bytes32 oldRoot;
            bytes32 newRoot;
            uint64 startIndex;
            uint64 actualCount;
            bytes32[8] cms;
            uint256[2][8] cvDeps;
            uint64[8] leafAsset;
            uint64[8] leafPublicIn;
            uint8[8] isDeposit;
        }
        /// `PubInputs.SpendTree`: the part of a spend's tree update the relayer
        /// supplies. The pool rebuilds the rest of the `TreeUpdateBatch` image
        /// from the spend itself, with `oldRoot = currentRoot()`.
        struct SpendTree {
            bytes32 newRoot;
            /// Must equal `committedCount()`.
            uint64 startIndex;
            /// Ring slot of `Transact.merkleRoot`: the pool checks
            /// `roots[anchorIndex] == merkleRoot`. Not a public input.
            uint8 anchorIndex;
        }
        /// `PubInputs.DepositRequest`. A deposit occupies two leaves: the
        /// depositor's, whose `cvDep` the batch circuit pins to `publicIn` units
        /// of `publicAssetId` under blinder `rcv`, and the fee note, pinned to
        /// `feeIn` units of `feeAssetId` under `feeRcv`.
        struct DepositRequest {
            uint256 chainId;
            uint64 publicAssetId;
            uint64 publicIn;
            address payer;
            address recipient;
            bytes32 outCm;
            uint256[2] cvDep;
            uint256 rcv;
            // The relayer's fee note, the second leaf every deposit mints.
            // Appended so the existing prefix keeps its ABI offsets.
            // `feeAssetId` may differ from `publicAssetId`; it is 0 exactly
            // when `feeIn` is 0.
            uint64 feeAssetId;
            uint64 feeIn;
            bytes32 feeCm;
            uint256[2] feeCvDep;
            uint256 feeRcv;
        }
        /// Digest fields the contract does not store, replayed at flush time and
        /// verified against `escrowed[id]`. Sourced from the deposit's
        /// `DepositEscrowed` event, where `submittedAt` is its block number.
        struct DepositMeta {
            address payer;
            uint32 submittedAt;
            uint16 fbps;
        }
        struct OutputAux {
            uint256 clueRx;
            uint256 clueRy;
            uint256 ephPubX;
            uint256 ephPubY;
            bytes ciphertext;
        }
        /// `maxFee` bounds the second token of a two-token (cross-asset)
        /// Permit2 batch; it must be 0 on the single-token path.
        struct Permit2Sig {
            uint256 nonce;
            uint256 deadline;
            uint256 maxTotal;
            uint256 maxFee;
            bytes signature;
        }
        /// `PubInputs.FeeNote`: the fee leaf's digest preimage, as `cancelDeposit`
        /// takes it.
        struct FeeNote {
            uint48 feeIn;
            uint64 feeAssetId;
            bytes32 feeCm;
            uint256[2] feeCvDep;
        }

        function currentRoot() external view returns (bytes32);
        /// Ring slot of `currentRoot()`; the `j`-th accepted root since genesis
        /// sits at slot `j mod 64`.
        function rootIndex() external view returns (uint32);
        /// Ring slot `i`, `0..64`; an unfilled slot holds zero.
        function roots(uint256 i) external view returns (bytes32);
        /// Read at boot, so a dry run can replace their code with an
        /// always-accepting stub; see `pipeline::batcher`.
        function TREE_UPDATE_BATCH_VERIFIER() external view returns (address);
        function SPEND_VERIFIER() external view returns (address);
        /// Escrow storage collapsed to a single digest; every other field lives
        /// off-chain in `DepositMeta`.
        function escrowed(uint256 id) external view returns (bytes32 digest);

        function deposit(
            DepositRequest calldata d,
            Permit2Sig calldata sig,
            OutputAux calldata aux,
            OutputAux calldata feeAux
        ) external returns (uint256 id);

        function depositAuthorized(
            DepositRequest calldata d,
            OutputAux calldata aux,
            OutputAux calldata feeAux
        ) external returns (uint256 id);

        function flushBatch(
            uint256[] calldata ids,
            DepositMeta[] calldata meta,
            Proof calldata tp,
            TreeUpdateBatch calldata tpi
        ) external;

        function cancelDeposit(
            uint256 id,
            uint48 publicIn,
            bytes32 cm,
            uint256[2] calldata cvDep,
            uint64 publicAssetId,
            uint16 fbps,
            address payer,
            uint32 submittedAt,
            FeeNote calldata fee
        ) external returns (uint256 total, uint256 feeRefunded);

        function transfer(
            Proof calldata p,
            Transact calldata pi,
            Proof calldata tp,
            SpendTree calldata tpi,
            OutputAux[6] calldata aux
        ) external;

        function withdraw(
            Proof calldata p,
            Transact calldata pi,
            Proof calldata tp,
            SpendTree calldata tpi,
            OutputAux[6] calldata aux
        ) external;
    }

    /// SwapWrapper ABI. The field layout must match
    /// `contracts/src/swap/SwapWrapper.sol :: SwapArgs`. The wrapper's internal
    /// `IMASPSwap.Proof` is ABI-identical to `IMasp.Proof`, which is referenced
    /// here to avoid duplicate types and a per-leg copy.
    interface ISwapWrapper {
        struct SwapArgs {
            address tokenIn;
            address tokenOut;
            uint256 amountIn;
            uint256 minOut;
            address adapter;
            bytes route;
            uint256 deadline;
            /// Where a cancelled output escrow is refunded. Bound through
            /// `pi_w.intentHash`; the wrapper reverts `InvalidRefundTo` on zero or
            /// itself.
            address refundTo;
            IMasp.Proof p_w;
            IMasp.Transact pi_w;
            IMasp.Proof tp_w;
            IMasp.SpendTree tpi_w;
            IMasp.OutputAux[6] aux_w;
            IMasp.DepositRequest deposit_d;
            /// Two leaves per deposit, hence two aux payloads; leg 1's withdraw
            /// carries one per transact output.
            IMasp.OutputAux aux_d;
            /// Fee-note payload for the B-note deposit's second leaf. The swap
            /// pays the relayer on its withdraw leg, so this is usually a
            /// zero-value pad (`feeIn = 0`, `feeAssetId = 0`), but it is still a
            /// leaf and still part of the digest preimage. A valued one must be
            /// in the output asset (`feeAssetId == deposit_d.publicAssetId`).
            IMasp.OutputAux fee_aux_d;
            /// The A note the wrapper escrows the unshield back into when the
            /// venue leg fails, with its two leaves' payloads. Bound through
            /// `pi_w.intentHash` like `deposit_d`.
            IMasp.DepositRequest refund_d;
            IMasp.OutputAux refund_aux_d;
            IMasp.OutputAux refund_fee_aux_d;
        }

        function swap(SwapArgs calldata a) external returns (uint256 actualOut, uint256 depositId);
    }

    /// This relayer's submission contract, `contracts/src/bundler/Bundler.sol`.
    /// Every tree-advancing call goes through it, so it is the pool's and the
    /// swap wrapper's `msg.sender`, and the address wallets bind as their
    /// submitter. `execute` makes the calls in order and stops at the first
    /// failure, keeping the calls before it; it reverts only on a malformed bundle
    /// or an unauthorised caller.
    interface IBundler {
        struct Call {
            address target;
            bytes data;
        }

        event BundleExecuted(uint256 executed, uint256 total);
        event BundleItemFailed(uint256 indexed index, bytes reason);

        function execute(Call[] calldata calls) external returns (uint256 executed, bytes memory reason);
    }

    /// Native-coin bridge. MASP is ERC-20 only, so unwrapping lives here. Both
    /// `pi.recipient` and `pi.relayer` must be the adapter address: the adapter
    /// drives `MASP.withdraw` itself and forwards the unwrapped proceeds to
    /// `pi.payer`.
    interface INativeAdapter {
        function withdrawNative(
            IMasp.Proof calldata p,
            IMasp.Transact calldata pi,
            IMasp.Proof calldata tp,
            IMasp.SpendTree calldata tpi,
            IMasp.OutputAux[6] calldata aux
        ) external returns (uint256 net);
    }
}

#[cfg(test)]
mod tests {
    use super::{IBundler, IMasp, INativeAdapter, ISwapWrapper};
    use alloy::sol_types::SolCall;

    /// Selectors of every call the relayer sends, as the contracts compile them
    /// (`forge inspect <Contract> methodIdentifiers`). The `sol!` block is a
    /// hand-written copy, and a drifted field changes the selector: the Bundler
    /// then refuses the call as `CallNotAllowed` and the whole bundle reverts.
    #[test]
    fn call_selectors_match_the_contracts() {
        for (name, selector, expected) in [
            ("MASP.transfer", IMasp::transferCall::SELECTOR, "ccef4c72"),
            ("MASP.withdraw", IMasp::withdrawCall::SELECTOR, "5df701cb"),
            (
                "MASP.flushBatch",
                IMasp::flushBatchCall::SELECTOR,
                "2bd77bd8",
            ),
            ("MASP.rootIndex", IMasp::rootIndexCall::SELECTOR, "529dd5ea"),
            ("MASP.roots", IMasp::rootsCall::SELECTOR, "c2b40ae4"),
            (
                "MASP.currentRoot",
                IMasp::currentRootCall::SELECTOR,
                "fdab463d",
            ),
            ("MASP.escrowed", IMasp::escrowedCall::SELECTOR, "34918bde"),
            ("MASP.deposit", IMasp::depositCall::SELECTOR, "fee3714c"),
            (
                "MASP.depositAuthorized",
                IMasp::depositAuthorizedCall::SELECTOR,
                "df1daf3b",
            ),
            (
                "MASP.cancelDeposit",
                IMasp::cancelDepositCall::SELECTOR,
                "5a0083a7",
            ),
            (
                "NativeAdapter.withdrawNative",
                INativeAdapter::withdrawNativeCall::SELECTOR,
                "dc30d670",
            ),
            (
                "SwapWrapper.swap",
                ISwapWrapper::swapCall::SELECTOR,
                "942bc9b9",
            ),
            (
                "Bundler.execute",
                IBundler::executeCall::SELECTOR,
                "baae8abf",
            ),
        ] {
            assert_eq!(hex::encode(selector), expected, "{name}");
        }
    }
}
