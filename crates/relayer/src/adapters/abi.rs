#![allow(clippy::too_many_arguments)]
use alloy::sol;

sol! {
    #![sol(rpc)]
    /// MASP, SwapWrapper, GenericCallWrapper and NativeAdapter ABIs. The field
    /// layout must match `contracts/src/MASP.sol`,
    /// `contracts/src/libs/PubInputs.sol`, `contracts/src/swap/*.sol`,
    /// `contracts/src/generic/*.sol` and
    /// `contracts/src/native/NativeAdapter.sol`. Declared in one `sol!`
    /// invocation so the wrappers and the adapter can reference
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
            /// Zero unless `publicOut != 0`: a transfer names no asset.
            uint64 publicAssetId;
            uint64 publicOut;
            /// The circuit's Poseidon commitment to the thirteen words above,
            /// which are the coefficients. Hashed into `z`, not evaluated into
            /// `y`, and handed to the verifier as given.
            uint256 digest;
            /// Challenge-only: hashed into `z`, never evaluated into `y`.
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
        ///
        /// `cms[k]` is the note commitment on a spend leaf and the depositor's
        /// `inner` on a deposit leaf; see `PaddedBatch::leaves`.
        struct TreeUpdateBatch {
            bytes32 oldRoot;
            bytes32 newRoot;
            uint64 startIndex;
            uint64 actualCount;
            bytes32[8] cms;
            uint64[8] leafAsset;
            uint64[8] leafPublicIn;
            uint8[8] isDeposit;
            /// The batch circuit's Poseidon commitment to the 36 words above.
            uint256 digest;
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
            /// The batch circuit's digest over the 36 coefficients of the batch
            /// this spend implies.
            uint256 digest;
        }
        /// `PubInputs.DepositRequest`. A deposit occupies two leaves: the
        /// depositor's, which the batch circuit builds from `publicIn` units of
        /// `publicAssetId` and `inner`, and the fee note, built from `feeIn`
        /// units of `feeAssetId` and `feeInner`.
        struct DepositRequest {
            uint256 chainId;
            uint64 publicAssetId;
            uint64 publicIn;
            address payer;
            address recipient;
            /// `Poseidon(TAG_INNER, pk, rho, rcm)` of the depositor's note.
            bytes32 inner;
            // The relayer's fee note, the second leaf every deposit mints.
            // `feeAssetId` may differ from `publicAssetId`; it is 0 exactly
            // when `feeIn` is 0.
            uint64 feeAssetId;
            uint64 feeIn;
            bytes32 feeInner;
        }
        /// Digest fields the contract does not store, replayed at flush time and
        /// verified against `escrowed[id]`. Sourced from the deposit's
        /// `DepositEscrowed` event, where `submittedAt` is its block number.
        struct DepositMeta {
            address payer;
            uint32 submittedAt;
            uint16 fbps;
            /// `DepositEscrowed.pulled`; see `PendingDeposit::pulled`.
            uint256 pulled;
        }
        /// `AuxValidation.Output`. `clueQ` is the subgroup witness for the
        /// clue: the pool checks `[8]·Q == R`.
        struct OutputAux {
            uint256 clueRx;
            uint256 clueRy;
            uint256 clueQx;
            uint256 clueQy;
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
            bytes32 feeInner;
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
            bytes32 inner,
            uint64 publicAssetId,
            uint16 fbps,
            address payer,
            uint32 submittedAt,
            FeeNote calldata fee,
            uint256 pulled
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

    /// GenericCallWrapper ABI. The field layout must match
    /// `contracts/src/generic/GenericCallWrapper.sol :: GenericArgs` and
    /// `contracts/src/generic/CallExecutor.sol :: Call`.
    interface IGenericCallWrapper {
        /// `CallExecutor.Call`. `value` is paid from the executing clone's own
        /// native balance.
        struct Call {
            address target;
            uint256 value;
            bytes data;
        }
        /// One shielded output: a note of the registry token of
        /// `deposit.publicAssetId`, escrowed for at least `minOut`.
        struct Output {
            uint256 minOut;
            IMasp.DepositRequest deposit;
            IMasp.OutputAux aux;
            IMasp.OutputAux feeAux;
        }
        struct GenericArgs {
            /// Floor on what the withdraw delivers. Not part of the intent.
            uint256 amountIn;
            Call[] calls;
            Output[] outputs;
            uint256 deadline;
            /// Gas the call leg must be forwarded; `execute` reverts
            /// `InsufficientGas` below it.
            uint256 minGas;
            /// Where a cancelled escrow is refunded.
            address refundTo;
            /// Receives slippage cushions, unused input and native leftovers.
            address surplusTo;
            IMasp.Proof p_w;
            IMasp.Transact pi_w;
            IMasp.Proof tp_w;
            IMasp.SpendTree tpi_w;
            IMasp.OutputAux[6] aux_w;
            /// The note the input is escrowed back into when the call leg
            /// fails, with its two leaves' payloads.
            IMasp.DepositRequest refund_d;
            IMasp.OutputAux refund_aux_d;
            IMasp.OutputAux refund_fee_aux_d;
        }

        function execute(GenericArgs calldata a) external returns (uint256[] memory depositIds);
    }

    /// This relayer's submission contract, `contracts/src/bundler/Bundler.sol`.
    /// Every tree-advancing call goes through it, so it is the pool's and the
    /// wrappers' `msg.sender`, and the address wallets bind as their
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
    use super::{IBundler, IGenericCallWrapper, IMasp, INativeAdapter, ISwapWrapper};
    use alloy::sol_types::SolCall;

    /// Selectors of every call the relayer sends, as the contracts compile them
    /// (`forge inspect <Contract> methodIdentifiers`). The `sol!` block is a
    /// hand-written copy, and a drifted field changes the selector: the Bundler
    /// then refuses the call as `CallNotAllowed` and the whole bundle reverts.
    #[test]
    fn call_selectors_match_the_contracts() {
        for (name, selector, expected) in [
            ("MASP.transfer", IMasp::transferCall::SELECTOR, "fb6accd7"),
            ("MASP.withdraw", IMasp::withdrawCall::SELECTOR, "6199cd11"),
            (
                "MASP.flushBatch",
                IMasp::flushBatchCall::SELECTOR,
                "f5ab0489",
            ),
            ("MASP.rootIndex", IMasp::rootIndexCall::SELECTOR, "529dd5ea"),
            ("MASP.roots", IMasp::rootsCall::SELECTOR, "c2b40ae4"),
            (
                "MASP.currentRoot",
                IMasp::currentRootCall::SELECTOR,
                "fdab463d",
            ),
            ("MASP.escrowed", IMasp::escrowedCall::SELECTOR, "34918bde"),
            ("MASP.deposit", IMasp::depositCall::SELECTOR, "8969b932"),
            (
                "MASP.depositAuthorized",
                IMasp::depositAuthorizedCall::SELECTOR,
                "4778b347",
            ),
            (
                "MASP.cancelDeposit",
                IMasp::cancelDepositCall::SELECTOR,
                "c4e85ddc",
            ),
            (
                "NativeAdapter.withdrawNative",
                INativeAdapter::withdrawNativeCall::SELECTOR,
                "20f6a331",
            ),
            (
                "SwapWrapper.swap",
                ISwapWrapper::swapCall::SELECTOR,
                "2037231e",
            ),
            (
                "GenericCallWrapper.execute",
                IGenericCallWrapper::executeCall::SELECTOR,
                "2431355d",
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
