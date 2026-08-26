//! FMD cryptographic primitives.
//!
//! Poseidon, Baby Jubjub, the FMD filter test, the append-only Merkle tree, and
//! note commitments with trial decryption. Pure computation, no IO. Must not be
//! imported by `explorer-indexer` or `explorer-webserver`; the privacy gate is
//! enforced in CI.

pub mod babyjubjub;
pub mod clue;
pub mod filter;
pub mod note;
pub mod poseidon;
pub mod tree;

pub use clue::{CircomPoint, test_clue as test_clue_point};
pub use filter::test_clue;
