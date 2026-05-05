use std::sync::Arc;

use ant_core::data::Client as CoreClient;
use autvid_common::HttpMetrics;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) client: Arc<CoreClient>,
    pub(crate) network: String,
    pub(crate) metrics: Arc<HttpMetrics>,
}
