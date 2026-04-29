// Emit `VERGEN_GIT_SHA` consumed by `option_env!` at compile time. No-op for
// builds outside a git repo (e.g. release tarball) — caller falls back to
// "unknown".

use vergen_gix::{Emitter, GixBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let gix = GixBuilder::all_git()?;
    Emitter::default().add_instructions(&gix)?.emit()?;
    Ok(())
}
