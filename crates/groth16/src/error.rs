use thiserror::Error;

/// What can go wrong proving or verifying, split by whose fault it is.
///
/// The split is the whole point of a separate type: this crate parses both a
/// verification key a deployment ships and a proof a caller sends, with the same
/// curve-point code. A binary mapping this onto an HTTP status must not report a
/// broken key as a bad request, which would blame whichever caller arrived
/// first, nor a malformed proof as an internal error, which would hide the fix
/// from the only party who can apply it.
#[derive(Debug, Error)]
pub enum Groth16Error {
    /// Proving failed: a missing zkey or graph, a witness the circuit rejects,
    /// or a proof that would not verify against its own public inputs.
    #[error("prover: {0}")]
    Prove(String),
    /// The prover was busy and the caller declined to queue. Only
    /// [`crate::Priority::Flush`] yields this way.
    #[error("prover busy")]
    Busy,
    /// A key file this deployment ships is unreadable, malformed, or describes a
    /// different circuit. Operator-side.
    #[error("verification key: {0}")]
    Key(String),
    /// Verification could not be carried out, as opposed to a proof that was
    /// carried out and failed — which is [`Groth16Error::InvalidProof`].
    #[error("verify: {0}")]
    Verify(String),
    /// The proof is malformed or does not verify against the public inputs it
    /// claims. Caller-side.
    #[error("{0}")]
    InvalidProof(String),
}

pub type Groth16Result<T> = Result<T, Groth16Error>;

/// Turn a foreign error into a [`Groth16Error::Prove`] with context in front of
/// it, so a failure names the step it came from rather than only its cause.
///
/// The context is a `&'static str`: this is used inside the prover's per-signal
/// loops, where building a `String` per call would cost more than the operation
/// it describes. Where an error needs runtime detail, use `map_err` with a
/// closure so the formatting stays on the failure path.
pub(crate) trait ErrorContext<T> {
    fn prover(self, step: &'static str) -> Groth16Result<T>;
}

impl<T, E: std::fmt::Display> ErrorContext<T> for Result<T, E> {
    fn prover(self, step: &'static str) -> Groth16Result<T> {
        self.map_err(|e| Groth16Error::Prove(format!("{step}: {e}")))
    }
}
