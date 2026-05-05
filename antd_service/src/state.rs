use std::sync::Arc;

use ant_core::data::Client as CoreClient;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) client: Arc<CoreClient>,
    pub(crate) network: String,
}
