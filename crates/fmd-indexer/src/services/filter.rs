use crate::domain::convert::{bigdec_to_fq, clue_bits_be};
use crate::domain::error::{FmdIndexerError, Result};
use crate::repositories::cursor::{CursorRepo, UpsertCursor};
use crate::repositories::matches::{MatchesRepo, NewMatch};
use crate::repositories::notes::{NoteRow, NotesRepo};
use crate::repositories::subscriptions::{SubscriptionRow, SubscriptionsRepo};
use ark_ed_on_bn254::{Fq, Fr};
use async_trait::async_trait;
use fmd_crypto::clue::CircomPoint;
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::sync::Arc;
use tracing::{debug, info};

pub const NAME: &str = "fmd-filter";

#[async_trait]
pub trait FilterService: Send + Sync {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<()>;
    async fn list_chain_ids(&self) -> Vec<i64>;
}

pub struct FilterServiceImpl {
    cursors: Arc<dyn CursorRepo>,
    notes: Arc<dyn NotesRepo>,
    subscriptions: Arc<dyn SubscriptionsRepo>,
    matches: Arc<dyn MatchesRepo>,
}

impl FilterServiceImpl {
    pub fn new(
        cursors: Arc<dyn CursorRepo>,
        notes: Arc<dyn NotesRepo>,
        subscriptions: Arc<dyn SubscriptionsRepo>,
        matches: Arc<dyn MatchesRepo>,
    ) -> Self {
        Self {
            cursors,
            notes,
            subscriptions,
            matches,
        }
    }

    async fn advance_cursor(&self, chain_id: i64, last_id: i64, last_block: i64) -> Result<()> {
        self.cursors
            .upsert(UpsertCursor {
                name: NAME.to_string(),
                chain_id,
                last_event_id: last_id,
                last_block_number: last_block,
            })
            .await?;
        Ok(())
    }
}

#[async_trait]
impl FilterService for FilterServiceImpl {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<()> {
        let (after_note_id, _) = self.cursors.fetch(NAME, chain_id).await?;
        let new_notes = self
            .notes
            .fetch_after(chain_id, after_note_id, batch)
            .await?;
        if new_notes.is_empty() {
            return Ok(());
        }

        let last_id = new_notes.last().map(|n| n.id).unwrap_or(after_note_id);
        let last_block = new_notes.last().map(|n| n.block_number).unwrap_or(0);

        let subs = self.subscriptions.list_active().await?;
        if subs.is_empty() {
            self.advance_cursor(chain_id, last_id, last_block).await?;
            return Ok(());
        }

        let hits = scan(&new_notes, &subs, chain_id).await?;

        self.matches.insert_batch(&hits).await?;
        self.advance_cursor(chain_id, last_id, last_block).await?;
        let hits_len = hits.len();
        debug!(
            chain_id,
            candidates = new_notes.len(),
            subs = subs.len(),
            hits = hits_len,
            last_id,
            last_block,
            "filter tick"
        );
        if hits_len > 0 {
            info!(
                chain_id,
                hits = hits_len,
                subs = subs.len(),
                last_id,
                "filter matches"
            );
        }
        Ok(())
    }

    async fn list_chain_ids(&self) -> Vec<i64> {
        self.cursors.list_chain_ids().await.unwrap_or_default()
    }
}

#[async_trait]
impl shared::tick::TickService for FilterServiceImpl {
    fn name(&self) -> &'static str {
        NAME
    }
    async fn list_chain_ids(&self) -> Vec<i64> {
        FilterService::list_chain_ids(self).await
    }
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> anyhow::Result<()> {
        FilterService::tick_chain(self, chain_id, batch)
            .await
            .map_err(Into::into)
    }
}

/// Cartesian product of (note × subscription), evaluated in parallel via
/// rayon on a blocking task. clueBits = first 2 bytes (BE) of ciphertext.
async fn scan(notes: &[NoteRow], subs: &[SubscriptionRow], chain_id: i64) -> Result<Vec<NewMatch>> {
    let note_inputs: Arc<[(i64, Fq, Fq, u16)]> = notes
        .iter()
        .filter_map(|n| {
            let rx = bigdec_to_fq(&n.clue_rx);
            let ry = bigdec_to_fq(&n.clue_ry);
            if !CircomPoint::new(rx, ry).is_on_curve() {
                return None;
            }
            let bits = clue_bits_be(&n.ciphertext).unwrap_or(0);
            Some((n.id, rx, ry, bits))
        })
        .collect::<Vec<_>>()
        .into();

    type SubEntry = (i64, Arc<[Fr]>, usize);
    let sub_inputs: Arc<[SubEntry]> = subs
        .iter()
        .filter_map(|s| {
            let gamma = s.gamma as usize;
            let dk = fmd_crypto::filter::parse_detection_key(&s.detection_key, gamma)?;
            Some((s.id, Arc::<[Fr]>::from(dk), gamma))
        })
        .collect::<Vec<_>>()
        .into();

    tokio::task::spawn_blocking(move || {
        let subs = &*sub_inputs;
        // Group by gamma so each group can run as a single batch per note:
        // one fixed-base table per (note, gamma) amortizes scalar muls
        // across the whole subscriber set.
        let mut by_gamma: std::collections::BTreeMap<usize, Vec<usize>> =
            std::collections::BTreeMap::new();
        for (idx, (_, _, g)) in subs.iter().enumerate() {
            by_gamma.entry(*g).or_default().push(idx);
        }

        let per_note = |(nid, rx, ry, bits): &(i64, Fq, Fq, u16)| -> Vec<NewMatch> {
            let mut hits: Vec<NewMatch> = Vec::new();
            for (gamma, indices) in by_gamma.iter() {
                let dks: Vec<&[Fr]> = indices.iter().map(|&i| subs[i].1.as_ref()).collect();
                let res = fmd_crypto::filter::test_clue_batch_parsed(&dks, *rx, *ry, *bits, *gamma);
                for (k, hit) in res.iter().enumerate() {
                    if *hit {
                        hits.push(NewMatch {
                            subscription_id: subs[indices[k]].0,
                            note_id: *nid,
                            chain_id,
                        });
                    }
                }
            }
            hits
        };

        #[cfg(feature = "parallel")]
        {
            note_inputs
                .par_iter()
                .flat_map_iter(|n| per_note(n).into_iter())
                .collect()
        }
        #[cfg(not(feature = "parallel"))]
        {
            note_inputs.iter().flat_map(|n| per_note(n)).collect()
        }
    })
    .await
    .map_err(|e| FmdIndexerError::Crypto(e.to_string()))
}
