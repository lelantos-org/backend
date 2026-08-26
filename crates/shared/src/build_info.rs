//! Build-time identity, declared once per crate by [`build_info!`].
//!
//! `env!` and `option_env!` read the environment of the crate being compiled, so
//! the constants have to be expanded at the call site; a plain function here
//! would report `shared`'s own identity for every binary.

/// Fallback for a `VERGEN_*` variable the build script did not set.
pub const UNKNOWN: &str = "unknown";

/// Resolve an optional build-script variable.
pub const fn or_unknown(value: Option<&'static str>) -> &'static str {
    match value {
        Some(v) => v,
        None => UNKNOWN,
    }
}

/// Declare this crate's build identity and a `log_banner()` that emits it.
///
/// Defines `PKG_NAME`, `PKG_VERSION`, `GIT_SHA`, `GIT_DESCRIBE`,
/// `GIT_COMMIT_TIMESTAMP`, `BUILD_TIMESTAMP`, `RUSTC_SEMVER` and
/// `CARGO_TARGET_TRIPLE`.
///
/// The `VERGEN_*` values come from the crate's own `build.rs`. A build outside a
/// git checkout, such as from a release tarball, leaves them unset and they read
/// [`UNKNOWN`] rather than failing to compile.
#[macro_export]
macro_rules! build_info {
    () => {
        pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");
        pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
        pub const GIT_SHA: &str = $crate::build_info::or_unknown(option_env!("VERGEN_GIT_SHA"));
        pub const GIT_DESCRIBE: &str =
            $crate::build_info::or_unknown(option_env!("VERGEN_GIT_DESCRIBE"));
        pub const GIT_COMMIT_TIMESTAMP: &str =
            $crate::build_info::or_unknown(option_env!("VERGEN_GIT_COMMIT_TIMESTAMP"));
        pub const BUILD_TIMESTAMP: &str =
            $crate::build_info::or_unknown(option_env!("VERGEN_BUILD_TIMESTAMP"));
        pub const RUSTC_SEMVER: &str =
            $crate::build_info::or_unknown(option_env!("VERGEN_RUSTC_SEMVER"));
        pub const CARGO_TARGET_TRIPLE: &str =
            $crate::build_info::or_unknown(option_env!("VERGEN_CARGO_TARGET_TRIPLE"));

        /// Log what this process was built from, once at startup.
        pub fn log_banner() {
            $crate::tracing::info!(
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
    };
}
