use crate::adapters::{TokenKey, TokenPrice};
use crate::app::AppState;
use std::collections::HashMap;

pub use prices::to_usd;

/// [`prices::for_tokens`] bound to this crate's state.
pub async fn for_tokens(st: &AppState, keys: &[TokenKey]) -> HashMap<TokenKey, TokenPrice> {
    prices::for_tokens(&st.prices, &st.cache.prices, keys).await
}
