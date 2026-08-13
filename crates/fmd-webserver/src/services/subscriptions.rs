use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::SubscriptionOut;
use crate::domain::token::TokenHash;
use crate::repositories::{
    notes,
    subscriptions::{self, SubscriptionRow},
};
use std::sync::Arc;

/// γ sets the false-positive rate at `2^-γ`. The circuit carries
/// out_clue_bits as a plain PolyEval-bound public input with no in-circuit
/// constraints; the upper bits above γ are masked by the contract (0x3FFF).
/// Higher γ = lower FP rate; lower γ = more privacy via false positives.
pub const GAMMA_MIN: i32 = 1;
pub const GAMMA_MAX: i32 = 16;

/// How many false positives a subscription's match set must be expected to
/// contain. The decoys are the only thing standing between the stored
/// `matches` rows and an exact user → note map, so γ cannot be chosen
/// independently of how many notes exist to draw decoys from: at γ=16 with
/// fewer than 65k notes the expected decoy count drops below one and the
/// match set *is* the user's note set.
const MIN_EXPECTED_DECOYS: i64 = 64;

/// Largest γ that still yields `MIN_EXPECTED_DECOYS` false positives against
/// a pool of `note_count` notes, clamped to the protocol range.
fn max_gamma_for(note_count: i64) -> i32 {
    // Largest γ with 2^γ <= note_count / MIN_EXPECTED_DECOYS. `ilog2` panics
    // on zero, hence the floor.
    match note_count / MIN_EXPECTED_DECOYS {
        budget if budget < 2 => GAMMA_MIN,
        budget => (budget.ilog2() as i32).clamp(GAMMA_MIN, GAMMA_MAX),
    }
}

fn not_found() -> AppError {
    AppError::NotFound("subscription".to_string())
}

/// Resolve a caller-supplied capability token to the internal subscription id.
pub async fn id_for_token(st: &AppState, token: &TokenHash) -> AppResult<i64> {
    subscriptions::id_by_token(&st.pool, token)
        .await?
        .ok_or_else(not_found)
}

/// Total note count, memoised for the cache TTL. See `AppCache::note_count`
/// for why this must not hit the database per request.
async fn note_count(st: &AppState) -> AppResult<i64> {
    let pool = st.pool.clone();
    st.cache
        .note_count
        .try_get_with((), async move { notes::count_all(&pool).await })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

/// Check γ against the protocol range and against the current note count,
/// then decode the detection key it must describe. Returns the key bytes.
async fn validate(st: &AppState, dk_hex: &str, gamma: i32) -> AppResult<Vec<u8>> {
    if !(GAMMA_MIN..=GAMMA_MAX).contains(&gamma) {
        return Err(AppError::BadRequest(format!(
            "gamma must be in {GAMMA_MIN}..={GAMMA_MAX}, got {gamma}"
        )));
    }

    let max_gamma = max_gamma_for(note_count(st).await?);
    if gamma > max_gamma {
        return Err(AppError::BadRequest(format!(
            "gamma must be <= {max_gamma} at the current note count so the \
             match set keeps at least {MIN_EXPECTED_DECOYS} expected false \
             positives, got {gamma}"
        )));
    }

    let dk = hex::decode(dk_hex.trim_start_matches("0x"))
        .map_err(|e| AppError::BadRequest(format!("invalid hex: {e}")))?;
    let expected = gamma as usize * 32;
    if dk.len() != expected {
        return Err(AppError::BadRequest(format!(
            "detection_key length must be gamma*32 = {expected}, got {}",
            dk.len()
        )));
    }
    Ok(dk)
}

fn output(row: &SubscriptionRow, created: bool) -> SubscriptionOut {
    SubscriptionOut {
        gamma: row.gamma,
        active: row.active,
        created,
    }
}

/// Registration under an existing token is idempotent only when the request
/// describes the same subscription. Any other request is rejected: updating
/// the detection key in place would repoint the row at the new caller's key
/// while the original owner still holds the token, exposing that owner's
/// match stream.
fn reattach(existing: &SubscriptionRow, dk: &[u8], gamma: i32) -> AppResult<SubscriptionOut> {
    if existing.detection_key != dk || existing.gamma != gamma {
        return Err(AppError::Conflict("token already registered".to_string()));
    }
    Ok(output(existing, false))
}

#[tracing::instrument(skip(st, dk_hex, token_hex), fields(gamma))]
pub async fn create(
    st: &AppState,
    dk_hex: &str,
    gamma: i32,
    token_hex: &str,
) -> AppResult<SubscriptionOut> {
    let dk = validate(st, dk_hex, gamma).await?;
    let token = TokenHash::registered(token_hex)?;

    // Insert first and let the unique index on `token_hash` arbitrate. It is
    // the authority on token ownership, so no concurrent registration can
    // slip past the write.
    //
    // The row lands with `backfilled_through_note_id = 0`; the indexer's
    // filter worker walks it forward over history one batch at a time. No
    // shared cursor is rewound, so registering cannot force a rescan for
    // every other subscriber.
    if let Some(row) = subscriptions::create(&st.pool, dk.clone(), gamma, &token).await? {
        return Ok(output(&row, true));
    }

    // The token is taken. Derived tokens are stable across registrations, so
    // this is the path a wallet takes when re-registering after losing local
    // state: it resolves to the existing subscription rather than a duplicate
    // that would backfill from scratch.
    let existing = subscriptions::find_by_token(&st.pool, &token)
        .await?
        .ok_or_else(|| AppError::Conflict("token deleted mid-registration; retry".to_string()))?;
    reattach(&existing, &dk, gamma)
}

#[tracing::instrument(skip(st, token))]
pub async fn delete(st: &AppState, token: &TokenHash) -> AppResult<()> {
    match subscriptions::delete_by_token(&st.pool, token).await? {
        0 => Err(not_found()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(dk: &[u8], gamma: i32) -> SubscriptionRow {
        SubscriptionRow {
            id: 1,
            detection_key: dk.to_vec(),
            gamma,
            created_at: chrono::Utc::now(),
            active: true,
            backfilled_through_note_id: 0,
        }
    }

    #[test]
    fn same_subscription_reattaches() {
        let dk = vec![1u8; 160];
        let out = reattach(&row(&dk, 5), &dk, 5).expect("same dk and gamma");
        assert!(!out.created);
        assert_eq!(out.gamma, 5);
    }

    #[test]
    fn a_taken_token_cannot_be_repointed() {
        let owner_dk = vec![1u8; 160];
        let attacker_dk = vec![2u8; 160];
        // Accepting this would serve the owner's matches to the caller that
        // supplied the new key, under a token the owner still holds.
        assert!(matches!(
            reattach(&row(&owner_dk, 5), &attacker_dk, 5),
            Err(AppError::Conflict(_))
        ));
        // Same key, different gamma: a different subscription.
        assert!(matches!(
            reattach(&row(&owner_dk, 5), &owner_dk, 6),
            Err(AppError::Conflict(_))
        ));
    }

    #[test]
    fn gamma_ceiling_tracks_note_volume() {
        // Not enough notes to hide anyone: only the most permissive γ.
        assert_eq!(max_gamma_for(0), GAMMA_MIN);
        assert_eq!(max_gamma_for(127), GAMMA_MIN);

        // 128 notes / 64 decoys = budget 2 -> 2^1.
        assert_eq!(max_gamma_for(128), 1);
        // 1024 / 64 = 16 -> 2^4.
        assert_eq!(max_gamma_for(1024), 4);
        // 65536 / 64 = 1024 -> 2^10.
        assert_eq!(max_gamma_for(65_536), 10);
    }

    #[test]
    fn gamma_ceiling_never_exceeds_protocol_max() {
        assert_eq!(max_gamma_for(i64::MAX), GAMMA_MAX);
    }

    #[test]
    fn gamma_ceiling_keeps_the_decoy_floor() {
        for notes in [128_i64, 1_024, 65_536, 10_000_000] {
            let g = max_gamma_for(notes);
            let expected_decoys = notes / (1_i64 << g);
            assert!(
                expected_decoys >= MIN_EXPECTED_DECOYS || g == GAMMA_MIN,
                "gamma {g} at {notes} notes leaves only {expected_decoys} decoys"
            );
        }
    }
}
