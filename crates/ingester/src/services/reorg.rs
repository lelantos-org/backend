use crate::domain::error::IngesterError;
use crate::domain::models::{BlockCursor, RawEvent};
use crate::repositories::{ChainStateRepo, RawEventRepo};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

pub struct ReorgService {
    raw_events: Arc<dyn RawEventRepo>,
    chain_state: Arc<dyn ChainStateRepo>,
}

impl ReorgService {
    pub fn new(raw_events: Arc<dyn RawEventRepo>, chain_state: Arc<dyn ChainStateRepo>) -> Self {
        Self {
            raw_events,
            chain_state,
        }
    }

    /// Returns Some(rewind_to) when stored block_hash mismatches incoming.
    pub async fn detect(
        &self,
        chain_id: i64,
        incoming: &[RawEvent],
    ) -> Result<Option<i64>, IngesterError> {
        if incoming.is_empty() {
            return Ok(None);
        }
        let mut by_block: HashMap<i64, Vec<u8>> = HashMap::new();
        for r in incoming {
            by_block
                .entry(r.block_number)
                .or_insert_with(|| r.block_hash.clone());
        }
        for (bn, hash) in by_block {
            if let Some(stored) = self.raw_events.block_hash_at(chain_id, bn).await?
                && stored != hash
            {
                warn!(chain_id, bn, "parent-hash mismatch");
                return Ok(Some(bn));
            }
        }
        Ok(None)
    }

    pub async fn rewind(&self, chain_id: i64, from_block: i64) -> Result<(), IngesterError> {
        info!(chain_id, from_block, "rewinding chain state");
        let deleted = self
            .raw_events
            .delete_from_block(chain_id, from_block)
            .await?;
        let new_scan = (from_block - 1).max(0);
        info!(chain_id, deleted, new_scan, "rewind applied");
        self.chain_state
            .upsert(BlockCursor {
                chain_id,
                last_block: new_scan,
                last_block_hash: vec![],
                last_scanned_block: new_scan,
            })
            .await?;
        Ok(())
    }
}
