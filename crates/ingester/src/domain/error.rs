use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("range too large")]
    RangeTooLarge,
    #[error("response too large")]
    ResponseTooLarge,
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
