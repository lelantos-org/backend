//! The contracts services read through, and Multicall3's default address.

use alloy::primitives::{Address, address};
use alloy::sol;

/// Multicall3's canonical address.
///
/// Deterministic-deployed via Nick's method, so the address is a property of the
/// bytecode rather than of the deployer and is identical on every chain that has
/// it. It is a *default*, not a guarantee: a bare `anvil` has no Multicall3, and
/// a chain that deployed its own has it elsewhere — callers take the address
/// from config and use this when none is set.
pub const MULTICALL3: Address = address!("0xcA11bde05977b3631167028862bE2a173976CA11");

sol! {
    /// The aggregator every consumer of this crate batches reads through.
    ///
    /// Here rather than in one adapter because it is not one service's business:
    /// `explorer-indexer` batches its yield round through it today, and the
    /// relayer's per-deposit `digests` fan-out and the metaquoter's fee-tier race
    /// are the same shape. A second copy would mean a second 20-byte address to
    /// get right, and a wrong one does not error — it reads as "no Multicall3"
    /// and silently runs slow.
    #[sol(rpc)]
    interface IMulticall3 {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }
        struct Result {
            bool success;
            bytes returnData;
        }
        function aggregate3(Call3[] calldata calls) external payable returns (Result[] memory returnData);
        function getBlockNumber() external view returns (uint256 blockNumber);
    }
}

sol! {
    /// The read surface of a yield venue. Must match
    /// `contracts/src/yield/IYieldVenue.sol`.
    ///
    /// Unioned here from two half-declarations that could not see each other:
    /// `protocol-indexer` polled `totalAssets`/`POOL` while `protocol-webserver`
    /// read `VAULT`, so a change to the contract had two places to drift and
    /// neither crate held the whole interface.
    ///
    /// Read-only on purpose: moving a position is the pool's job and `onlyPool`
    /// would reject anything here, so nothing that writes belongs in it.
    ///
    /// Unlike the events in `events.rs`, these declarations are **not** covered by
    /// `tests/sig_check.rs` — the contract ships no ABI JSON into this repo, so
    /// there is nothing independent to pin a selector against. See that file.
    #[sol(rpc)]
    interface IYieldVenue {
        /// The ERC-4626 vault this venue holds shares of. Immutable, which is
        /// what lets a caller resolve it once and keep it.
        function VAULT() external view returns (address);
        function totalAssets() external view returns (uint256);
        function POOL() external view returns (address);
    }

    /// The optional ERC-20 metadata calls the asset catalog fills in from chain.
    ///
    /// `AssetRegistered` carries `scale`, a circuit capacity parameter rather
    /// than a decimals normalizer, so rendering a human-readable amount needs the
    /// token's own `decimals()`.
    #[sol(rpc)]
    interface IERC20Metadata {
        function decimals() external view returns (uint8);
        /// Optional in ERC-20, and some early tokens return `bytes32` rather than
        /// `string`, which does not decode here. A caller leaves the column NULL
        /// and retries.
        function symbol() external view returns (string);
        /// Read off an ERC-4626 vault's share token, whose label is what tells
        /// an earning asset from the plain asset sharing its underlying.
        function name() external view returns (string);
    }

    /// The ERC-4626 surface the venue rate estimate reads.
    ///
    /// `convertToAssets` is the vault's share price: two readings of the same
    /// probe, at two blocks, are the whole measurement. `decimals` sizes that
    /// probe.
    #[sol(rpc)]
    interface IERC4626 {
        function convertToAssets(uint256 shares) external view returns (uint256);
        function decimals() external view returns (uint8);
    }
}
