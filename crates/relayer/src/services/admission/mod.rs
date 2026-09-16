//! Admission control for note-spending submissions, run before a pipeline
//! spends anything on them.
//!
//! - `idempotency`: a resubmission under the same `Idempotency-Key` replays
//!   the first answer rather than spending twice.
//! - `nullifier_guard`: a payload spending a nullifier already in flight or
//!   already spent is refused before it is proved.
//!
//! Both are single-process state, like the tree mirror they protect.

pub mod idempotency;
pub mod nullifier_guard;
