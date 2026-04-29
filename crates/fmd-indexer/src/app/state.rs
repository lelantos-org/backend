use crate::services::{ConsumeService, FilterService};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub consume: Arc<dyn ConsumeService>,
    pub filter: Arc<dyn FilterService>,
}
