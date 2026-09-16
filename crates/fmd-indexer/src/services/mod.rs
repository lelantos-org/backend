pub mod consume;
pub mod filter;

pub use consume::{ConsumeService, ConsumeServiceImpl};
pub use filter::{FilterService, FilterServiceImpl};

use database::CursorRepo;
use tracing::warn;

/// The chains a tick should visit this round: every chain the shared cursor
/// table knows about.
///
/// Both loops answer `TickService::list_chain_ids` with this. An empty list is
/// indistinguishable from no chains being configured, so a failed read is
/// logged rather than idled through.
async fn chain_ids(cursors: &dyn CursorRepo) -> Vec<i64> {
    match cursors.list_chain_ids().await {
        Ok(ids) => ids,
        Err(e) => {
            warn!(error = %e, "list_chain_ids failed; skipping this round");
            Vec::new()
        }
    }
}
