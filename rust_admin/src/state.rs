use std::sync::{atomic::AtomicU64, Arc};

use sqlx::PgPool;
use tokio::sync::{Mutex, Semaphore};

use crate::{antd_client::AntdRestClient, config::Config, metrics::AdminMetrics};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Arc<Config>,
    pub(crate) pool: PgPool,
    pub(crate) antd: AntdRestClient,
    pub(crate) metrics: Arc<AdminMetrics>,
    pub(crate) catalog_lock: Arc<Mutex<()>>,
    pub(crate) catalog_publish_lock: Arc<Mutex<()>>,
    pub(crate) catalog_publish_epoch: Arc<AtomicU64>,
    pub(crate) upload_save_semaphore: Arc<Semaphore>,
}
