//! Run FMD detection over ingested notes.
//!
//! Two passes per tick: `forward` scans notes that arrived since this chain's
//! cursor against every active subscription, and `backfill` walks one lagging
//! subscription over history. Unlocked, unlike consume, because `matches`
//! inserts are idempotent and both pointers only move forward.
//!
//! - `head`: the lagged `notes.id` head the backfill pages up to.
//! - `subscribers`: the parsed active subscriber set, cached by fingerprint.
//! - `scan`: the note-by-subscriber detection pass.

mod head;
mod scan;
mod subscribers;

use crate::domain::error::Result;
use crate::repositories::matches::{MatchesRepo, NewMatch};
use crate::repositories::notes::NotesRepo;
use crate::repositories::subscriptions::SubscriptionsRepo;
use async_trait::async_trait;
use database::{CursorRepo, UpsertCursor};
use head::LaggedHead;
use scan::{ScanStats, group_by_chain, scan};
use shared::tick::TickProgress;
use std::sync::Arc;
use subscribers::{SubEntry, SubscriberSet, UNUSABLE_KEY, sub_entry};
use tokio::sync::Mutex;
use tracing::{debug, warn};

pub const NAME: &str = "fmd-filter";

#[async_trait]
pub trait FilterService: Send + Sync {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<TickProgress>;
    async fn list_chain_ids(&self) -> Vec<i64>;
}

pub struct FilterServiceImpl {
    cursors: Arc<dyn CursorRepo>,
    notes: Arc<dyn NotesRepo>,
    subscriptions: Arc<dyn SubscriptionsRepo>,
    matches: Arc<dyn MatchesRepo>,
    head: Mutex<LaggedHead>,
    subscribers: Mutex<Option<Arc<SubscriberSet>>>,
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
            subscribers: Mutex::new(None),
        }
    }

    /// The active subscriber set, parsed, reused until the table changes.
    ///
    /// Reading and parsing it per tick made both costs linear in subscriber
    /// count on every pass: the rows carry a detection key each, and
    /// `parse_detection_key` decompresses `gamma` points per key. The
    /// fingerprint query touches no key and reads no row payload, so the common
    /// tick pays one aggregate instead.
    async fn subscribers(&self) -> Result<Arc<SubscriberSet>> {
        let fingerprint = self.subscriptions.active_fingerprint().await?;
        let mut slot = self.subscribers.lock().await;
        if let Some(cached) = slot.as_ref().filter(|c| c.matches(fingerprint)) {
            return Ok(cached.clone());
        }

        let rows = self.subscriptions.list_active().await?;
        let set = Arc::new(SubscriberSet::build(fingerprint, &rows));
        *slot = Some(set.clone());
        Ok(set)
    }

    /// Scan the notes ingested since this chain's cursor against every active
    /// subscription, then advance the cursor past them.
    async fn forward_tick(&self, chain_id: i64, batch: i64) -> Result<TickProgress> {
        let (after_note_id, _) = self.cursors.fetch(NAME, chain_id).await?;
        record_cursor(chain_id, after_note_id);
        let new_notes = self
            .notes
            .fetch_after(chain_id, after_note_id, batch)
            .await?;
        let Some(last) = new_notes.last() else {
            return Ok(TickProgress::Idle);
        };
        // A full batch means more notes are already waiting behind it.
        let progress = TickProgress::from_batch(new_notes.len(), batch);
        let (last_id, last_block) = (last.id, last.block_number);

        let subs = self.subscribers().await?;
        if subs.entries.is_empty() {
            self.advance_cursor(chain_id, last_id, last_block).await?;
            return Ok(progress);
        }

        let outcome = scan(&new_notes, &subs.entries, chain_id).await?;
        self.matches.insert_batch(&outcome.hits).await?;
        self.advance_cursor(chain_id, last_id, last_block).await?;

        subs.warn_unusable();
        // Emitted unconditionally and at debug. Logging only when hits > 0 would
        // make the log stream a receive-timing side channel; the skip counts are
        // unconditional for the same reason.
        debug!(
            chain_id,
            candidates = new_notes.len(),
            subs = subs.entries.len(),
            hits = outcome.hits.len(),
            off_curve_notes = outcome.stats.off_curve_notes,
            invalid_subs = subs.invalid.len(),
            last_id,
            last_block,
            "filter tick"
        );
        Ok(progress)
    }

    /// Walk one lagging subscription forward over history by a single batch.
    ///
    /// Registering a subscription does not rewind the shared cursor, so a burst
    /// of registrations costs one batch per tick rather than a rescan of all
    /// history against every subscriber. The pointer is a global `notes.id`, so
    /// this pass is chain-agnostic; running it from several per-chain ticks only
    /// converges faster, and re-scanning an overlapping range is absorbed by
    /// `ON CONFLICT DO NOTHING`.
    async fn backfill_tick(&self, batch: i64) -> Result<TickProgress> {
        let max_id = self.notes.max_id().await?;
        record_notes_head(max_id);
        let head = self.head.lock().await.observe(max_id);
        let Some(sub) = self.subscriptions.next_backfilling(head).await? else {
            return Ok(TickProgress::Idle);
        };
        let sub_id = sub.id;

        let mut notes = self
            .notes
            .fetch_after_any_chain(sub.backfilled_through_note_id, batch)
            .await?;
        // `fetch_after_any_chain` has no upper bound, so a batch can reach past
        // the safe head into ids that may still be interleaved with an
        // uncommitted one.
        notes.retain(|n| n.id <= head);
        let Some(through) = notes.last().map(|n| n.id) else {
            // Nothing left below `head` for this subscription: mark it caught up
            // so it stops being picked. That retires one subscription and the
            // next tick may find another, so report progress rather than idle.
            self.subscriptions.advance_backfill(sub_id, head).await?;
            return Ok(TickProgress::Partial);
        };

        let candidates = notes.len();
        // `retain` above may have trimmed the batch below `head`, so saturation
        // is measured on what is actually scanned.
        let progress = TickProgress::from_batch(candidates, batch);
        let Some(entry) = sub_entry(&sub) else {
            // A key that does not parse matches nothing, so the pointer is moved
            // past this range rather than retried forever on the same rows.
            warn!(subscription_id = sub_id, "{UNUSABLE_KEY}");
            self.subscriptions.advance_backfill(sub_id, through).await?;
            return Ok(progress);
        };
        let subs: Arc<[SubEntry]> = vec![entry].into();
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

        debug!(
            candidates,
            hits = hits.len(),
            off_curve_notes = stats.off_curve_notes,
            through,
            head,
            "filter backfill"
        );
        Ok(progress)
    }

    /// Monotonic but not advisory-locked, unlike the consume loop. `matches`
    /// inserts are idempotent (primary key plus `ON CONFLICT DO NOTHING`), and
    /// the one hazard — a note slipping below the cursor because `notes.id` is
    /// assigned before commit — requires concurrent writers for this chain, which
    /// the consume lock rules out. See [`BACKFILL_LAG`](head::BACKFILL_LAG) for why the global
    /// backfill pointer lacks the same guarantee.
    async fn advance_cursor(&self, chain_id: i64, last_id: i64, last_block: i64) -> Result<()> {
        // Discarded: this loop is unlocked and several per-chain ticks race to
        // advance the same cursor, so losing the race is expected.
        let _ = self
            .cursors
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

/// Cursor half of the notes-to-matches lag pair.
///
/// `notes` carries no `block_ts`, so this hop cannot report a wall-clock age the
/// way ingest and consume do. The distance between this and
/// [`record_notes_head`] is the available signal.
fn record_cursor(chain_id: i64, after_note_id: i64) {
    metrics::gauge!(
        shared::metrics::name::CONSUMER_CURSOR_NOTE_ID,
        "service" => NAME,
        "chain_id" => chain_id.to_string(),
    )
    .set(after_note_id as f64);
}

/// Head half of the pair; see [`record_cursor`].
///
/// Unlabelled by chain because `max_id` is global, making the difference an upper
/// bound on any one chain's lag rather than that chain's lag. Emitted from the
/// backfill pass, which already runs the query.
fn record_notes_head(max_id: i64) {
    metrics::gauge!(shared::metrics::name::NOTES_MAX_ID).set(max_id as f64);
}

#[async_trait]
impl FilterService for FilterServiceImpl {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<TickProgress> {
        // Both passes run every tick, and the driver must not sleep while either
        // has queued work, so take the higher of the two.
        let forward = self.forward_tick(chain_id, batch).await?;
        let backfill = self.backfill_tick(batch).await?;
        Ok(forward.max(backfill))
    }

    async fn list_chain_ids(&self) -> Vec<i64> {
        super::chain_ids(self.cursors.as_ref()).await
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
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> anyhow::Result<TickProgress> {
        FilterService::tick_chain(self, chain_id, batch)
            .await
            .map_err(Into::into)
    }
}
