#![allow(clippy::too_many_arguments)]
use alloy::sol;

sol! {
    #![sol(rpc)]
    /// MASP, SwapWrapper and NativeAdapter ABIs. The field layout must match
    /// `contracts/src/MASP.sol`, `contracts/src/libs/PubInputs.sol`,
    /// `contracts/src/swap/*.sol` and
    /// `contracts/src/native/NativeAdapter.sol`. Declared in one `sol!`
    /// invocation so the wrapper and the adapter can reference
    /// `IMasp.{Proof, Transact, TreeUpdateBatch, OutputAux, DepositRequest}`
    /// without duplicating types.
    interface IMasp {
        struct Proof {
            uint256[2] a;
            uint256[2][2] b;
            uint256[2] c;
        }
        /// `PubInputs.Transact`. The array arity is the deployed circuit's
        /// `TRANSACT_IN` and `TRANSACT_OUT`, both 4. `sol!` takes literals, so
        /// the arity cannot be written in terms of the Rust constants;
        /// `domain::dto::transact` asserts the pair at compile time. Changing the
        /// arity requires a new circuit, ceremony and verifier.
        struct Transact {
            bytes32 merkleRoot;
            bytes32[4] nullifier;
            bytes32[4] outCm;
            uint64 publicAssetId;
            uint64 publicIn;
            uint64 publicOut;
            uint256[2][4] inCv;
            uint256[2][4] outCv;
            address recipient;
            uint256 chainId;
            address payer;
            address relayer;
            uint256[2][4] outCvDep;
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
            bytes32[4] cms;
            uint256[2][4] cvDeps;
            uint64[4] leafAsset;
            uint64[4] leafPublicIn;
            uint8[4] isDeposit;
        }
        /// `PubInputs.DepositRequest`. A deposit occupies exactly one leaf,
        /// whose `cvDep` the batch circuit pins to `publicIn` units of
        /// `publicAssetId` under blinder `rcv`.
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
        struct Permit2Sig {
            uint256 nonce;
            uint256 deadline;
            uint256 maxTotal;
            bytes signature;
        }

        function currentRoot() external view returns (bytes32);
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
            uint48 feeIn,
            bytes32 feeCm,
            uint256[2] calldata feeCvDep
        ) external;

        function transfer(
            Proof calldata p,
            Transact calldata pi,
            Proof calldata tp,
            TreeUpdateBatch calldata tpi,
            OutputAux[4] calldata aux
        ) external;

        function withdraw(
            Proof calldata p,
            Transact calldata pi,
            Proof calldata tp,
            TreeUpdateBatch calldata tpi,
            OutputAux[4] calldata aux
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
            IMasp.Proof p_w;
            IMasp.Transact pi_w;
            IMasp.Proof tp_w;
            IMasp.TreeUpdateBatch tpi_w;
            IMasp.OutputAux[4] aux_w;
            IMasp.DepositRequest deposit_d;
            /// Two leaves per deposit, hence two aux payloads; leg 1's withdraw
            /// carries one per transact output.
            IMasp.OutputAux aux_d;
            /// Fee-note payload for the B-note deposit's second leaf. The swap
            /// pays the relayer on its withdraw leg, so this is a zero-value pad,
            /// but it is still a leaf and still part of the digest preimage.
            IMasp.OutputAux fee_aux_d;
        }

        function swap(SwapArgs calldata a) external returns (uint256 actualOut, uint256 depositId);
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
            IMasp.TreeUpdateBatch calldata tpi,
            IMasp.OutputAux[4] calldata aux
        ) external returns (uint256 net);
    }
}
