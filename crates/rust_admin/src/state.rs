use std::sync::{atomic::AtomicU64, Arc};

use sqlx::SqlitePool;
use tokio::sync::watch;
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::{antd_client::AntdRestClient, config::Config, metrics::AdminMetrics};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: SqlitePool,
    pub antd: AntdRestClient,
    pub metrics: Arc<AdminMetrics>,
    pub catalog_lock: Arc<Mutex<()>>,
    pub catalog_publish_lock: Arc<Mutex<()>>,
    pub catalog_publish_epoch: Arc<AtomicU64>,
    pub upload_save_semaphore: Arc<Semaphore>,
    pub shutdown: CancellationToken,
    pub job_notify_tx: watch::Sender<u64>,
}
