//! The price service, bound to this crate's state.

use crate::adapters::{TokenKey, TokenPrice};
use crate::app::AppState;
use std::collections::HashMap;

pub use prices::to_usd;

/// [`prices::PriceService::for_tokens`] bound to this crate's state.
pub async fn for_tokens(st: &AppState, keys: &[TokenKey]) -> HashMap<TokenKey, TokenPrice> {
    st.prices.for_tokens(keys).await
}
