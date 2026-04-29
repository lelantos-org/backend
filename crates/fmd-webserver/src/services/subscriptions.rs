use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::SubscriptionOut;
use crate::repositories::subscriptions;
use std::sync::Arc;

/// γ can be freely chosen per subscription (1–16). The circuit carries
/// out_clue_bits as a plain PolyEval-bound public input with no in-circuit
/// constraints; the upper bits above γ are masked by the contract (0x3FFF).
/// Higher γ = lower FP rate; lower γ = more privacy via false positives.
pub const GAMMA_MIN: i32 = 1;
pub const GAMMA_MAX: i32 = 16;

fn to_out(r: subscriptions::SubscriptionRow) -> SubscriptionOut {
    SubscriptionOut {
        id: r.id,
        detection_key_hex: hex::encode(&r.detection_key),
        gamma: r.gamma,
        active: r.active,
    }
}

#[tracing::instrument(skip(st))]
pub async fn list(st: &AppState) -> AppResult<Vec<SubscriptionOut>> {
    let pool = st.pool.clone();
    let cached = st
        .cache
        .subscriptions
        .try_get_with((), async move {
            let rows = subscriptions::list(&pool).await?;
            Ok::<_, AppError>(Arc::new(rows.into_iter().map(to_out).collect::<Vec<_>>()))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))?;
    Ok((*cached).clone())
}

#[tracing::instrument(skip(st, dk_hex), fields(gamma))]
pub async fn create(st: &AppState, dk_hex: &str, gamma: i32) -> AppResult<SubscriptionOut> {
    if !(GAMMA_MIN..=GAMMA_MAX).contains(&gamma) {
        return Err(AppError::BadRequest(format!(
            "gamma must be in {}..={}, got {}",
            GAMMA_MIN, GAMMA_MAX, gamma
        )));
    }
    let dk = hex::decode(dk_hex.trim_start_matches("0x"))
        .map_err(|e| AppError::BadRequest(format!("invalid hex: {}", e)))?;
    if dk.len() != (gamma as usize) * 32 {
        return Err(AppError::BadRequest(format!(
            "detection_key length must be gamma*32 = {}, got {}",
            gamma * 32,
            dk.len()
        )));
    }
    let out = to_out(subscriptions::create(&st.pool, dk, gamma).await?);
    // Without the cursor rewind, notes ingested before this subscription never get matched.
    subscriptions::reset_filter_cursor(&st.pool).await?;
    st.cache.subscriptions.invalidate(&()).await;
    Ok(out)
}

#[tracing::instrument(skip(st))]
pub async fn delete(st: &AppState, id: i64) -> AppResult<()> {
    let n = subscriptions::delete(&st.pool, id).await?;
    if n == 0 {
        return Err(AppError::NotFound(format!("subscription {}", id)));
    }
    st.cache.subscriptions.invalidate(&()).await;
    Ok(())
}
