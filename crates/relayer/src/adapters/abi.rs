#![allow(clippy::too_many_arguments)]
use alloy::sol;

sol! {
    #![sol(rpc)]
    /// MASP + SwapWrapper ABIs. Field layout MUST match `contracts/src/MASP.sol`
    /// + `contracts/src/lib/PubInputs.sol` + `contracts/src/swap/*.sol`.
    /// Two interfaces in one `sol!` invocation so SwapWrapper.SwapArgs
    /// can reference `IMasp.{Proof, Transact, TreeUpdateBatch, OutputAux,
    /// DepositIntent}` directly without duplicating types.
    interface IMasp {
        struct Proof {
            uint256[2] a;
            uint256[2][2] b;
            uint256[2] c;
        }
        struct Transact {
            bytes32 merkleRoot;
            bytes32[2] nullifier;
            bytes32[2] outCm;
            uint64 publicAssetId;
            uint64 publicIn;
            uint64 publicOut;
            uint256[2][2] inCv;
            uint256[2][2] outCv;
            address recipient;
            uint256 chainId;
            address payer;
            address relayer;
            uint256[2][2] outCvDep;
        }
        struct TreeUpdateBatch {
            bytes32 oldRoot;
            bytes32 newRoot;
            uint64 startIndex;
            uint64 actualCount;
            bytes32[16] cms;
            uint256[2][16] cvDeps;
            uint64[8] pairAsset;
            uint64[8] pairPublicIn;
            uint8[8] isDeposit;
        }
        struct DepositIntent {
            uint64 chainId;
            uint64 publicAssetId;
            uint64 publicIn;
            address payer;
            address recipient;
            bytes32[2] outCm;
            uint256[2] cvDep0;
            uint256[2] cvDep1;
            uint256 rcvTotal;
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
        function escrowed(uint256 id)
            external
            view
            returns (
                bytes32 digest,
                address payer,
                uint32 submittedAt,
                uint64 publicAssetId,
                uint16 feeBpsAtSubmit
            );

        function submitIntent(
            DepositIntent calldata d,
            Permit2Sig calldata sig,
            OutputAux[2] calldata aux
        ) external returns (uint256 id);

        function flushBatch(
            uint256[] calldata ids,
            Proof calldata tp,
            TreeUpdateBatch calldata tpi
        ) external;

        function cancelIntent(
            uint256 id,
            uint48 publicIn,
            bytes32 cm0,
            bytes32 cm1,
            uint256[2] calldata cvDep0,
            uint256[2] calldata cvDep1
        ) external;

        function transfer(
            Proof calldata p,
            Transact calldata pi,
            Proof calldata tp,
            TreeUpdateBatch calldata tpi,
            OutputAux[2] calldata aux
        ) external;

        function withdraw(
            Proof calldata p,
            Transact calldata pi,
            Proof calldata tp,
            TreeUpdateBatch calldata tpi,
            OutputAux[2] calldata aux
        ) external;

        function withdrawNative(
            Proof calldata p,
            Transact calldata pi,
            Proof calldata tp,
            TreeUpdateBatch calldata tpi,
            OutputAux[2] calldata aux
        ) external;

        event IntentEscrowed(
            uint256 indexed id,
            address indexed payer,
            address indexed recipient,
            uint64 publicAssetId,
            uint64 publicIn,
            uint16 feeBpsAtSubmit,
            bytes32 cm0,
            bytes32 cm1,
            uint256 cvDep0X,
            uint256 cvDep0Y,
            uint256 cvDep1X,
            uint256 cvDep1Y,
            uint256 rcvTotal,
            uint256 clueRx0,
            uint256 clueRy0,
            uint256 ephPubX0,
            uint256 ephPubY0,
            bytes ciphertext0,
            uint256 clueRx1,
            uint256 clueRy1,
            uint256 ephPubX1,
            uint256 ephPubY1,
            bytes ciphertext1
        );
        event IntentFlushed(uint256 indexed id, bytes32 cm0, bytes32 cm1);
        event IntentCanceled(uint256 indexed id, address indexed payer, uint256 refunded);
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
            IMasp.OutputAux[2] aux_w;
            IMasp.DepositIntent intent_d;
            IMasp.OutputAux[2] aux_d;
        }

        function swap(SwapArgs calldata a) external returns (uint256 actualOut, uint256 intentId);
    }
}
