use std::path::PathBuf;
use std::sync::Arc;

use crate::antd_client::AntdRestClient;
use crate::cache::AppCache;
use crate::config::CacheConfig;
use crate::metrics::StreamMetrics;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) antd: AntdRestClient,
    pub(crate) catalog_state_path: PathBuf,
    pub(crate) catalog_bootstrap_address: Option<String>,
    pub(crate) cache: Arc<AppCache>,
    pub(crate) cache_config: CacheConfig,
    pub(crate) metrics: Arc<StreamMetrics>,
}
