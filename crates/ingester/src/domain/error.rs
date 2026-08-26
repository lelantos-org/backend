use thiserror::Error;

/// What went wrong at the provider.
///
/// The variants drive behaviour rather than describe the wire format:
/// [`RpcError::RangeTooLarge`] narrows the query window and
/// [`RpcError::RateLimited`] backs off. Anything matching neither is
/// [`RpcError::Other`] and is retried.
#[derive(Debug, Error)]
pub enum RpcError {
    /// The provider refused the query as too wide, by block span or by response
    /// size. Both share one variant because both have the same remedy: request
    /// less.
    #[error("range too large")]
    RangeTooLarge,
    #[error("rate limited")]
    RateLimited,
    #[error("block {0} missing")]
    BlockMissing(u64),
    #[error("rpc: {0}")]
    Other(String),
}

#[derive(Debug, Error)]
pub enum IngesterError {
    #[error("config: {0}")]
    Config(String),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("db: {0}")]
    Db(String),
}

impl IngesterError {
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }
}

// Blanket conversions so repositories can use `?` rather than repeating a
// `map_err` on every statement. The pool's error type is a bb8 generic that
// nothing outside this module needs to name.
impl From<diesel::result::Error> for IngesterError {
    fn from(e: diesel::result::Error) -> Self {
        Self::Db(e.to_string())
    }
}

impl<E: std::error::Error + 'static> From<bb8::RunError<E>> for IngesterError {
    fn from(e: bb8::RunError<E>) -> Self {
        Self::Db(format!("pool: {}", e))
    }
}
