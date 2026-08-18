//! Run FMD detection over ingested notes.
//!
//! Two passes per tick: `forward` scans notes that arrived since this chain's
//! cursor against every active subscription, and `backfill` walks one lagging
//! subscription over history. Deliberately unlocked, unlike consume —
//! `matches` inserts are idempotent and both pointers only move forward.

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
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, warn};

pub const NAME: &str = "fmd-filter";

/// How long a `notes.id` must have been observable before the backfill pages
/// past it.
///
/// `notes.id` comes from a sequence, so it is allocated *before* commit and
/// ids do not become visible in id order. That is harmless for the per-chain
/// forward pass — the consume lock gives each chain one writer, so a chain's
/// own rows always appear in order — but the backfill walks a single *global*
/// pointer, and two replicas leading two chains do interleave. Reading the
/// head from `max(id)` therefore steps over a row that commits a moment later,
/// and the pointer never goes back: a note silently never scanned for that
/// subscription.
///
/// Lagging the head bounds the hazard by how long a single `INSERT` can stay
/// uncommitted instead of by id ordering. Five seconds against a sub-second
/// statement.
const BACKFILL_LAG: Duration = Duration::from_secs(5);

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
    head: Mutex<LaggedHead>,
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
            head: Mutex::new(LaggedHead::new()),
        }
    }

    /// Scan the notes ingested since this chain's cursor against every active
    /// subscription, then advance the cursor past them.
    async fn forward_tick(&self, chain_id: i64, batch: i64) -> Result<()> {
        let (after_note_id, _) = self.cursors.fetch(NAME, chain_id).await?;
        let new_notes = self
            .notes
            .fetch_after(chain_id, after_note_id, batch)
            .await?;
        let Some(last) = new_notes.last() else {
            return Ok(());
        };
        let (last_id, last_block) = (last.id, last.block_number);

        let subs = self.subscriptions.list_active().await?;
        if subs.is_empty() {
            return self.advance_cursor(chain_id, last_id, last_block).await;
        }

        let outcome = scan(&new_notes, &subs, chain_id).await?;
        self.matches.insert_batch(&outcome.hits).await?;
        self.advance_cursor(chain_id, last_id, last_block).await?;

        outcome.stats.warn_unusable();
        // Unconditional, and at debug: emitting only when hits > 0 turned the
        // log stream into a receive-timing side channel for anyone who could
        // read it. The skip counts are unconditional for the same reason.
        debug!(
            chain_id,
            candidates = new_notes.len(),
            subs = subs.len(),
            hits = outcome.hits.len(),
            off_curve_notes = outcome.stats.off_curve_notes,
            invalid_subs = outcome.stats.invalid_subs.len(),
            last_id,
            last_block,
            "filter tick"
        );
        Ok(())
    }

    /// Walk one lagging subscription forward over history by a single batch.
    ///
    /// Registering a subscription does not rewind the shared cursor, so a
    /// burst of registrations costs one batch per tick instead of a rescan of
    /// all history against every subscriber. The pointer is a global
    /// `notes.id`, so this pass is chain-agnostic; running it from several
    /// per-chain ticks only makes it converge faster, and re-scanning an
    /// overlapping range is absorbed by `ON CONFLICT DO NOTHING`.
    async fn backfill_tick(&self, batch: i64) -> Result<()> {
        let head = self.head.lock().await.observe(self.notes.max_id().await?);
        let Some(sub) = self.subscriptions.next_backfilling(head).await? else {
            return Ok(());
        };
        let sub_id = sub.id;

        let mut notes = self
            .notes
            .fetch_after_any_chain(sub.backfilled_through_note_id, batch)
            .await?;
        // `fetch_after_any_chain` has no upper bound, so a batch can reach
        // past the safe head into ids that may still be interleaved with an
        // uncommitted one.
        notes.retain(|n| n.id <= head);
        let Some(through) = notes.last().map(|n| n.id) else {
            // Nothing left below `head` for this subscription: mark it caught
            // up so it stops being picked.
            return self.subscriptions.advance_backfill(sub_id, head).await;
        };

        let candidates = notes.len();
        let subs = [sub];
        let mut hits: Vec<NewMatch> = Vec::new();
        let mut stats = ScanStats::default();
        // `scan` is per chain, but the pointer is a global note id, so a batch
        // can straddle chains.
        for (chain_id, chain_notes) in group_by_chain(notes) {
            let outcome = scan(&chain_notes, &subs, chain_id).await?;
            hits.extend(outcome.hits);
            stats.absorb(outcome.stats);
        }

        self.matches.insert_batch(&hits).await?;
        self.subscriptions.advance_backfill(sub_id, through).await?;

        stats.warn_unusable();
        debug!(
            candidates,
            hits = hits.len(),
            off_curve_notes = stats.off_curve_notes,
            through,
            head,
            "filter backfill"
        );
        Ok(())
    }

    /// Monotonic, but not advisory-locked, unlike the consume loop. `matches`
    /// inserts are idempotent (PK + `ON CONFLICT DO NOTHING`), and the one
    /// real hazard here — a note slipping below the cursor because `notes.id`
    /// is assigned before commit — needs concurrent writers *for this chain*,
    /// which the consume lock already rules out. See [`BACKFILL_LAG`] for why
    /// the global backfill pointer does not get the same guarantee for free.
    async fn advance_cursor(&self, chain_id: i64, last_id: i64, last_block: i64) -> Result<()> {
        self.cursors
            .upsert_monotonic(UpsertCursor {
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
        self.forward_tick(chain_id, batch).await?;
        self.backfill_tick(batch).await
    }

    async fn list_chain_ids(&self) -> Vec<i64> {
        match self.cursors.list_chain_ids().await {
            Ok(ids) => ids,
            Err(e) => {
                // An empty list is indistinguishable from "no chains
                // configured", so the loop would idle silently.
                warn!(error = %e, "list_chain_ids failed; skipping this round");
                Vec::new()
            }
        }
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

/// A `notes.id` head held back by [`BACKFILL_LAG`].
///
/// Keeps two observations: `safe` is old enough to page over, `pending` is
/// waiting out the lag. The clock is only reset on promotion, so calling this
/// many times per tick — the backfill runs once per chain — does not keep
/// pushing the promotion into the future.
struct LaggedHead {
    safe: i64,
    pending: i64,
    pending_since: Instant,
}

impl LaggedHead {
    fn new() -> Self {
        Self {
            safe: 0,
            pending: 0,
            pending_since: Instant::now(),
        }
    }

    fn observe(&mut self, max_id: i64) -> i64 {
        if self.pending_since.elapsed() >= BACKFILL_LAG {
            self.safe = self.pending;
            self.pending = max_id;
            self.pending_since = Instant::now();
        }
        self.safe
    }
}

/// Notes and subscriptions `scan` could not use.
///
/// Counted rather than dropped silently: a subscription whose detection key
/// does not parse matches nothing, forever, and nothing else in the system
/// would ever say so.
#[derive(Default)]
struct ScanStats {
    off_curve_notes: usize,
    /// A set, so folding per-chain scans of the same subscriber list together
    /// reports each id once.
    invalid_subs: BTreeSet<i64>,
}

impl ScanStats {
    fn absorb(&mut self, other: Self) {
        self.off_curve_notes += other.off_curve_notes;
        self.invalid_subs.extend(other.invalid_subs);
    }

    /// Ids only — never the key.
    fn warn_unusable(&self) {
        if !self.invalid_subs.is_empty() {
            warn!(
                subscription_ids = ?self.invalid_subs,
                "detection key is not gamma * 32 bytes; these subscriptions match nothing"
            );
        }
    }
}

struct ScanOutcome {
    hits: Vec<NewMatch>,
    stats: ScanStats,
}

/// Partition a note batch by chain, consuming it so no row is cloned.
fn group_by_chain(notes: Vec<NoteRow>) -> BTreeMap<i64, Vec<NoteRow>> {
    let mut by_chain: BTreeMap<i64, Vec<NoteRow>> = BTreeMap::new();
    for note in notes {
        by_chain.entry(note.chain_id).or_default().push(note);
    }
    by_chain
}

/// Cartesian product of (note × subscription), evaluated in parallel via
/// rayon on a blocking task. clueBits = first 2 bytes (BE) of ciphertext.
async fn scan(notes: &[NoteRow], subs: &[SubscriptionRow], chain_id: i64) -> Result<ScanOutcome> {
    let mut stats = ScanStats::default();

    let note_inputs: Arc<[(i64, Fq, Fq, u16)]> = notes
        .iter()
        .filter_map(|n| {
            let rx = bigdec_to_fq(&n.clue_rx);
            let ry = bigdec_to_fq(&n.clue_ry);
            if !CircomPoint::new(rx, ry).is_on_curve() {
                stats.off_curve_notes += 1;
                return None;
            }
            // `plan_commit` refuses to store a note whose ciphertext cannot
            // carry the prefix, so the fallback is unreachable for stored rows.
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
            let Some(dk) = fmd_crypto::filter::parse_detection_key(&s.detection_key, gamma) else {
                stats.invalid_subs.insert(s.id);
                return None;
            };
            Some((s.id, Arc::<[Fr]>::from(dk), gamma))
        })
        .collect::<Vec<_>>()
        .into();

    let hits = tokio::task::spawn_blocking(move || {
        let subs = &*sub_inputs;
        // Group by gamma so each group can run as a single batch per note:
        // one fixed-base table per (note, gamma) amortizes scalar muls
        // across the whole subscriber set.
        let mut by_gamma: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (idx, (_, _, g)) in subs.iter().enumerate() {
            by_gamma.entry(*g).or_default().push(idx);
        }

        // The key slices handed to `test_clue_batch_parsed` depend only on the
        // subscriber set, so build them once for the whole batch rather than
        // once per note — otherwise every note re-materialises a Vec as long
        // as the subscriber list.
        // (gamma, subscriber indices, their detection keys)
        type GammaGroup<'a> = (usize, Vec<usize>, Vec<&'a [Fr]>);
        let groups: Vec<GammaGroup> = by_gamma
            .into_iter()
            .map(|(gamma, indices)| {
                let dks: Vec<&[Fr]> = indices.iter().map(|&i| subs[i].1.as_ref()).collect();
                (gamma, indices, dks)
            })
            .collect();

        let per_note = |(nid, rx, ry, bits): &(i64, Fq, Fq, u16)| -> Vec<NewMatch> {
            let mut hits: Vec<NewMatch> = Vec::new();
            for (gamma, indices, dks) in &groups {
                let res = fmd_crypto::filter::test_clue_batch_parsed(dks, *rx, *ry, *bits, *gamma);
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
            note_inputs.iter().flat_map(per_note).collect()
        }
    })
    .await
    .map_err(|e| FmdIndexerError::Crypto(e.to_string()))?;

    Ok(ScanOutcome { hits, stats })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;

    fn note(id: i64, chain_id: i64) -> NoteRow {
        NoteRow {
            id,
            chain_id,
            block_number: 0,
            tx_hash: Vec::new(),
            log_index: 0,
            cm: Vec::new(),
            clue_rx: BigDecimal::from(0),
            clue_ry: BigDecimal::from(0),
            eph_pub_x: BigDecimal::from(0),
            eph_pub_y: BigDecimal::from(0),
            ciphertext: Vec::new(),
            leaf_index: 0,
            cv_dep_x: BigDecimal::from(0),
            cv_dep_y: BigDecimal::from(0),
        }
    }

    #[test]
    fn group_by_chain_partitions_a_straddling_batch() {
        // The backfill pointer is a global note id, so a batch can interleave
        // chains; every note must land in exactly one group, order preserved.
        let batch = vec![note(1, 10), note(2, 20), note(3, 10), note(4, 30)];

        let grouped = group_by_chain(batch);

        assert_eq!(grouped.keys().copied().collect::<Vec<_>>(), [10, 20, 30]);
        assert_eq!(ids(&grouped[&10]), [1, 3]);
        assert_eq!(ids(&grouped[&20]), [2]);
        assert_eq!(ids(&grouped[&30]), [4]);
    }

    #[test]
    fn group_by_chain_handles_an_empty_batch() {
        assert!(group_by_chain(Vec::new()).is_empty());
    }

    #[test]
    fn lagged_head_withholds_ids_until_they_have_aged() {
        let mut head = LaggedHead::new();

        // Ids written this instant are not offered: a peer's uncommitted row
        // could still be interleaved below them.
        assert_eq!(head.observe(100), 0);
        assert_eq!(head.observe(150), 0, "and the clock is not reset per call");

        head.pending_since -= BACKFILL_LAG;
        assert_eq!(head.observe(200), 0, "promotes the first observation");
        head.pending_since -= BACKFILL_LAG;
        assert_eq!(head.observe(250), 200, "which is now old enough to page");
    }

    fn ids(notes: &[NoteRow]) -> Vec<i64> {
        notes.iter().map(|n| n.id).collect()
    }
}
