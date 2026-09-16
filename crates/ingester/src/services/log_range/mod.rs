//! Adaptive `eth_getLogs` windowing.
//!
//! Providers cap how much one query may return, by block span, by response size
//! or both, and disagree on the limit and on how they report reaching it. Rather
//! than a per-provider constant, the cap is learned: narrow on rejection, and
//! once enough full-size windows have been served, probe back upward — the only
//! evidence available is the provider's own error wording, so the belief has to
//! be able to recover from misreading it.
//!
//! Once the cap is known the range splits into windows whose sizes need no
//! feedback from each other, so they are fetched concurrently under a budget
//! shared by every caller on the chain. Only the search itself is serial.

mod window;

pub use window::LogWindow;

use crate::adapters::DynRpc;
use crate::domain::decode::{distinct_blocks, logs_to_rows};
use crate::domain::error::{IngesterError, RpcError};
use crate::domain::models::RawEvent;
use alloy::primitives::Address;
use alloy::rpc::types::eth::Log;
use futures::stream::{self, StreamExt, TryStreamExt};
use shared::metrics::{ingest_stage, timed_ingest_stage};
use tracing::debug;
use window::Window;

/// Fetch every matching log in `[from, to]`, narrowing the query window to
/// whatever the provider will actually serve.
///
/// Anything that is not a range cap propagates: rate limits and transport errors
/// belong to the retry layer.
///
/// `learned` carries the provider's cap between calls, so only the first fetch
/// against a provider pays the halving search.
pub async fn fetch_adaptive(
    rpc: &DynRpc,
    learned: &LogWindow,
    address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<Log>, IngesterError> {
    let span = to.saturating_sub(from).saturating_add(1);
    let limit = learned.cap.limit();

    // A cap that is already known, and smaller than what was asked for, makes the
    // split arithmetic deterministic: none of the sub-window sizes depend on a
    // previous response, so they can go out together instead of the range being
    // walked one round trip at a time. Only the search itself needs feedback.
    if limit < span {
        match fetch_split(rpc, learned, address, from, to, limit).await {
            Ok(logs) => return Ok(logs),
            // The cap moved under us, or a probe overshot it. Fall through: the
            // serial walk re-reads the limit and re-learns it. Worth a line,
            // because the fallback re-fetches windows that had already landed
            // and an operator watching throughput deserves the reason.
            Err(IngesterError::Rpc(RpcError::RangeTooLarge)) => {
                debug!(
                    from,
                    to, limit, "log window rejected at the learned cap; re-probing"
                );
            }
            Err(e) => return Err(e),
        }
    }

    fetch_probing(rpc, learned, address, from, to).await
}

/// Fetch `[from, to]` as concurrent windows of `size`, which the provider is
/// already believed to accept.
///
/// Fails as a whole on a range rejection, leaving the caller to fall back: a
/// partial result cannot be stitched to a re-walk without tracking which windows
/// landed, and the case only arises when the learned cap is stale.
async fn fetch_split(
    rpc: &DynRpc,
    learned: &LogWindow,
    address: Address,
    from: u64,
    to: u64,
    size: u64,
) -> Result<Vec<Log>, IngesterError> {
    // `refuse` floors at one, so this only guards a caller passing its own size.
    let size = size.max(1);
    let starts = std::iter::successors(Some(from), move |&start| {
        let next = start.saturating_add(size);
        (next <= to).then_some(next)
    });

    stream::iter(starts.map(|start| {
        let end = start.saturating_add(size - 1).min(to);
        async move {
            let _permit = learned.permit().await;
            let logs = rpc.fetch_logs(address, start, end).await?;
            learned.cap.confirm(end - start + 1);
            Ok::<_, IngesterError>(logs)
        }
    }))
    // Ordered rather than unordered: the concurrency is already capped by the
    // provider's semaphore, so yielding in submission order costs nothing and
    // keeps a replayed range decoding identically.
    .buffered(learned.concurrency)
    // Folded rather than collected: a chunk spanning a busy contract holds every
    // window's logs at once either way, but collecting into a `Vec<Vec<Log>>` and
    // concatenating holds them twice.
    .try_fold(Vec::new(), |mut acc, logs| async move {
        absorb(&mut acc, logs);
        Ok(acc)
    })
    .await
}

/// Move one window's logs into the accumulator.
///
/// Taking the first response whole keeps the common case — a single window
/// covering the whole range — free of a copy.
fn absorb(acc: &mut Vec<Log>, mut logs: Vec<Log>) {
    if acc.is_empty() {
        *acc = logs;
    } else {
        acc.append(&mut logs);
    }
}

/// Walk `[from, to]` one window at a time, halving on rejection.
///
/// The path that learns the cap. Serial by necessity: each size is chosen from
/// the previous response.
async fn fetch_probing(
    rpc: &DynRpc,
    learned: &LogWindow,
    address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<Log>, IngesterError> {
    let span = to.saturating_sub(from).saturating_add(1);
    let mut window = Window::new(span, learned.cap.limit());
    let mut cursor = from;
    let mut acc = Vec::new();

    while cursor <= to {
        let end = cursor.saturating_add(window.size - 1).min(to);
        let result = {
            let _permit = learned.permit().await;
            rpc.fetch_logs(address, cursor, end).await
        };
        match result {
            Ok(logs) => {
                learned.cap.confirm(end - cursor + 1);
                absorb(&mut acc, logs);
                cursor = end + 1;
                // Re-read the limit rather than reusing the one sampled at
                // entry: a sibling chunk may have found the cap while this one
                // was in flight, and the first backfill wave would otherwise pay
                // the search once per concurrent chunk.
                window.grow(
                    to.saturating_sub(cursor).saturating_add(1),
                    learned.cap.limit(),
                );
            }
            Err(IngesterError::Rpc(RpcError::RangeTooLarge)) if window.can_shrink() => {
                window.shrink(&learned.cap);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(acc)
}

/// Fetch `[from, to]` and decode it into insertable rows.
///
/// The whole read side of a scan, shared by the live tick and the backfill so
/// the two cannot drift on stage labels or on how block metadata is resolved.
/// The caller decides what an empty result means: the live tick only advances
/// its watermark, while the backfill still commits the chunk.
pub async fn fetch_rows(
    rpc: &DynRpc,
    learned: &LogWindow,
    chain_id: i64,
    address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<RawEvent>, IngesterError> {
    let logs = timed_ingest_stage(
        ingest_stage::GET_LOGS,
        chain_id,
        fetch_adaptive(rpc, learned, address, from, to),
    )
    .await?;
    if logs.is_empty() {
        return Ok(Vec::new());
    }
    let block_meta = timed_ingest_stage(
        ingest_stage::BLOCK_META,
        chain_id,
        rpc.fetch_block_meta(&distinct_blocks(&logs)),
    )
    .await?;
    logs_to_rows(chain_id, logs, &block_meta)
}

#[cfg(test)]
mod tests;
