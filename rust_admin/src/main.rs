use std::{
    collections::BTreeSet,
    env, fs,
    sync::{atomic::AtomicU64, Arc},
    time::{Duration as StdDuration, Instant},
};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use tokio::{
    net::TcpListener,
    sync::Mutex,
    sync::Semaphore,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::antd_client::AntdRestClient;
use crate::config::Config;
use crate::db::ensure_schema;
use crate::jobs::{
    approval_cleanup_loop, cleanup_expired_approvals, recover_interrupted_jobs, start_job_workers,
};
use crate::state::AppState;

mod antd_client;
mod auth;
mod catalog;
mod config;
mod constants;
mod db;
mod errors;
mod jobs;
mod media;
mod metrics;
mod models;
mod pipeline;
mod quote;
mod routes;
mod state;
mod storage;
mod upload;

pub(crate) use constants::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if autvid_common::run_healthcheck_from_args(env::args())? {
        return Ok(());
    }

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Arc::new(Config::from_env()?);
    if let Some(parent) = config.db_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let db_connect_timeout =
        config::duration_from_secs_f64(config.admin_db_connect_timeout_seconds);
    let connect_options = SqliteConnectOptions::new()
        .filename(&config.db_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(db_connect_timeout);
    let pool_options = SqlitePoolOptions::new()
        .min_connections(config.admin_db_min_connections)
        .max_connections(config.admin_db_max_connections)
        .acquire_timeout(db_connect_timeout);
    let pool = tokio::time::timeout(
        db_connect_timeout,
        pool_options.connect_with(connect_options),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "SQLite pool connection timed out after {:.3}s",
            config.admin_db_connect_timeout_seconds
        )
    })??;
    ensure_schema(&pool).await?;

    fs::create_dir_all(&config.upload_temp_dir)?;
    if let Some(parent) = config.catalog_state_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let metrics = Arc::new(metrics::AdminMetrics::default());
    let shutdown = CancellationToken::new();
    let (job_notify_tx, _) = tokio::sync::watch::channel(0_u64);
    let antd = AntdRestClient::new(
        &config.antd_url,
        config.antd_upload_timeout_seconds.max(60.0) + 30.0,
        metrics.clone(),
        config.antd_internal_token.clone(),
    )?;
    ensure_autonomi_ready(&config, &antd).await?;

    let state = AppState {
        config: config.clone(),
        pool,
        antd,
        metrics,
        catalog_lock: Arc::new(Mutex::new(())),
        catalog_publish_lock: Arc::new(Mutex::new(())),
        catalog_publish_epoch: Arc::new(AtomicU64::new(0)),
        upload_save_semaphore: Arc::new(Semaphore::new(config.upload_max_concurrent_saves)),
        shutdown: shutdown.clone(),
        job_notify_tx,
    };
    cleanup_expired_approvals(&state).await?;
    recover_interrupted_jobs(state.clone()).await?;
    let mut background_tasks = start_job_workers(&state);
    background_tasks.push((
        "approval-cleanup".to_string(),
        tokio::spawn(approval_cleanup_loop(state.clone())),
    ));

    let app = routes::router(&config, state)?;

    let listener = TcpListener::bind(config.bind_addr).await?;
    info!("rust_admin listening on {}", config.bind_addr);
    let shutdown_signal_token = shutdown.clone();
    let server_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_signal_token))
        .await;
    shutdown.cancel();
    wait_for_background_tasks(
        background_tasks,
        config::duration_from_secs_f64(config.admin_shutdown_grace_seconds),
    )
    .await;
    server_result?;
    Ok(())
}

async fn shutdown_signal(shutdown: CancellationToken) {
    autvid_common::shutdown_signal().await;
    info!("shutdown signal received");
    shutdown.cancel();
}

async fn wait_for_background_tasks(
    tasks: Vec<(String, JoinHandle<()>)>,
    grace_period: StdDuration,
) {
    if tasks.is_empty() {
        return;
    }

    let mut pending = tasks
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    let mut joins = JoinSet::new();
    for (name, task) in tasks {
        joins.spawn(async move { (name, task.await) });
    }

    let deadline = Instant::now() + grace_period;
    while !pending.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, joins.join_next()).await {
            Ok(Some(Ok((name, Ok(()))))) => {
                pending.remove(&name);
                info!(task = %name, "background task stopped");
            }
            Ok(Some(Ok((name, Err(err))))) => {
                pending.remove(&name);
                warn!(task = %name, "background task failed during shutdown: {}", err);
            }
            Ok(Some(Err(err))) => {
                warn!("background task monitor failed during shutdown: {}", err);
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }

    for name in pending {
        warn!(
            task = %name,
            "background task did not stop before shutdown grace elapsed"
        );
    }
}

async fn ensure_autonomi_ready(config: &Config, antd: &AntdRestClient) -> anyhow::Result<()> {
    let status = antd.health().await?;
    if !status.status.eq_ignore_ascii_case("ok") {
        anyhow::bail!("antd health check returned not ok");
    }
    let wallet = antd.wallet_address().await?;
    let balance = antd.wallet_balance().await?;
    info!(
        wallet_address = %wallet.address,
        token_balance = %balance.balance,
        gas_balance = %balance.gas_balance,
        "Autonomi wallet ready"
    );
    if config.antd_approve_on_startup {
        let approved = antd.wallet_approve().await?;
        info!(
            approved = approved.approved,
            "Autonomi wallet spend approval checked"
        );
    }
    if config.antd_require_cost_ready {
        antd.data_cost_for_size(MIN_ANTD_SELF_ENCRYPTION_BYTES)
            .await?;
        info!("Autonomi write cost probe succeeded");
    }
    Ok(())
}
