#![allow(clippy::too_many_arguments)]
use alloy::sol;

sol! {
    #![sol(rpc)]
    /// MASP + SwapWrapper + NativeAdapter ABIs. Field layout MUST match
    /// `contracts/src/MASP.sol` + `contracts/src/libs/PubInputs.sol` +
    /// `contracts/src/swap/*.sol` + `contracts/src/native/NativeAdapter.sol`.
    /// One `sol!` invocation so the wrapper and the adapter can reference
    /// `IMasp.{Proof, Transact, TreeUpdateBatch, OutputAux, DepositRequest}`
    /// directly without duplicating types.
    interface IMasp {
        struct Proof {
            uint256[2] a;
            uint256[2][2] b;
            uint256[2] c;
        }
        /// `PubInputs.Transact` at the deployed 3-in/3-out shape. Changing
        /// the arity requires a new circuit, ceremony and verifier.
        struct Transact {
            bytes32 merkleRoot;
            bytes32[3] nullifier;
            bytes32[3] outCm;
            uint64 publicAssetId;
            uint64 publicIn;
            uint64 publicOut;
            uint256[2][3] inCv;
            uint256[2][3] outCv;
            address recipient;
            uint256 chainId;
            address payer;
            address relayer;
            uint256[2][3] outCvDep;
        }
        /// `PubInputs.TreeUpdateBatch`. Every array is indexed by LEAF, not
        /// by pair: `actualCount` is a leaf count in `[1, MAX_L_BATCH]`, so a
        /// batch may commit an odd number of leaves. Slots beyond
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
        }
        /// Digest fields the contract does not store, replayed at flush time
        /// and verified against `escrowed[id]`. Sourced from the deposit's
        /// `DepositEscrowed` event (`submittedAt` = its block number).
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
        /// Escrow storage collapsed to a single digest; every other field
        /// lives off-chain in `DepositMeta`.
        function escrowed(uint256 id) external view returns (bytes32 digest);

        function deposit(
            DepositRequest calldata d,
            Permit2Sig calldata sig,
            OutputAux calldata aux
        ) external returns (uint256 id);

        function depositAuthorized(
            DepositRequest calldata d,
            OutputAux calldata aux
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
            uint32 submittedAt
        ) external;

        function transfer(
            Proof calldata p,
            Transact calldata pi,
            Proof calldata tp,
            TreeUpdateBatch calldata tpi,
            OutputAux[3] calldata aux
        ) external;

        function withdraw(
            Proof calldata p,
            Transact calldata pi,
            Proof calldata tp,
            TreeUpdateBatch calldata tpi,
            OutputAux[3] calldata aux
        ) external;

        event DepositEscrowed(
            uint256 indexed id,
            address indexed payer,
            address indexed recipient,
            uint64 publicAssetId,
            uint64 publicIn,
            uint16 feeBpsAtSubmit,
            bytes32 cm,
            uint256 cvDepX,
            uint256 cvDepY,
            uint256 rcv,
            uint256 clueRx,
            uint256 clueRy,
            uint256 ephPubX,
            uint256 ephPubY,
            bytes ciphertext
        );
        event DepositFlushed(uint256 indexed id, bytes32 cm);
        event DepositCanceled(uint256 indexed id, address indexed payer, uint256 refunded);
    }

    /// SwapWrapper ABI. Field layout MUST match
    /// `contracts/src/swap/SwapWrapper.sol :: SwapArgs`. The wrapper's
    /// internal `IMASPSwap.Proof` is ABI-identical to `IMasp.Proof`; we
    /// reference the latter to avoid duplicate types and a per-leg copy.
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
            IMasp.OutputAux[3] aux_w;
            IMasp.DepositRequest deposit_d;
            /// One leaf per deposit, hence one aux payload; leg 1's withdraw
            /// carries one per transact output.
            IMasp.OutputAux aux_d;
        }

        function swap(SwapArgs calldata a) external returns (uint256 actualOut, uint256 depositId);
    }

    /// Native-coin bridge. MASP is ERC-20 only; unwrapping lives here.
    /// `pi.recipient` AND `pi.relayer` must both be the adapter address —
    /// the adapter drives `MASP.withdraw` itself and forwards the unwrapped
    /// proceeds to `pi.payer`.
    interface INativeAdapter {
        function withdrawNative(
            IMasp.Proof calldata p,
            IMasp.Transact calldata pi,
            IMasp.Proof calldata tp,
            IMasp.TreeUpdateBatch calldata tpi,
            IMasp.OutputAux[3] calldata aux
        ) external returns (uint256 net);
    }
}
