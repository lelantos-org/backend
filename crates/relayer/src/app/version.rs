// Build-time identity. `vergen-gix` emits the env vars from `build.rs`;
// missing git (no .git in build env) falls back to "unknown".

pub const CARGO_PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = match option_env!("VERGEN_GIT_SHA") {
    Some(v) => v,
    None => "unknown",
};
