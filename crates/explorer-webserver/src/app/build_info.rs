pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");
pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = env!("VERGEN_GIT_SHA");
pub const GIT_DESCRIBE: &str = env!("VERGEN_GIT_DESCRIBE");
pub const GIT_COMMIT_TIMESTAMP: &str = env!("VERGEN_GIT_COMMIT_TIMESTAMP");
pub const BUILD_TIMESTAMP: &str = env!("VERGEN_BUILD_TIMESTAMP");
pub const RUSTC_SEMVER: &str = env!("VERGEN_RUSTC_SEMVER");
pub const CARGO_TARGET_TRIPLE: &str = env!("VERGEN_CARGO_TARGET_TRIPLE");

pub fn log_banner() {
    tracing::info!(
        name = PKG_NAME,
        version = PKG_VERSION,
        git_sha = GIT_SHA,
        git_describe = GIT_DESCRIBE,
        git_commit_ts = GIT_COMMIT_TIMESTAMP,
        build_ts = BUILD_TIMESTAMP,
        rustc = RUSTC_SEMVER,
        target = CARGO_TARGET_TRIPLE,
        "starting"
    );
}
