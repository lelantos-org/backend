//! FMD cryptographic primitives.
//!
//! Poseidon, Baby Jubjub, FMD filter test, append-only Merkle tree, and note
//! commitments + trial decryption. Pure computation; no IO. Must NOT be imported by `explorer-indexer` or
//! `explorer-webserver` — privacy gate enforced in CI.

pub mod babyjubjub;
pub mod clue;
pub mod filter;
pub mod note;
pub mod poseidon;
pub mod tree;

pub use clue::{CircomPoint, test_clue as test_clue_point};
pub use filter::test_clue;
