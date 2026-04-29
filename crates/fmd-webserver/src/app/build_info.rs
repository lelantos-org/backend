pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");
pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = env!("VERGEN_GIT_SHA");
pub const GIT_DESCRIBE: &str = env!("VERGEN_GIT_DESCRIBE");
pub const GIT_COMMIT_TIMESTAMP: &str = env!("VERGEN_GIT_COMMIT_TIMESTAMP");

pub fn log_banner() {
    tracing::info!(
        name = PKG_NAME,
        version = PKG_VERSION,
        git_sha = GIT_SHA,
        git_describe = GIT_DESCRIBE,
        git_commit_ts = GIT_COMMIT_TIMESTAMP,
        "starting"
    );
}
