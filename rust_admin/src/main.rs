use std::{
    fs,
    sync::{atomic::AtomicU64, Arc},
};

use sqlx::postgres::PgPoolOptions;
use tokio::{net::TcpListener, sync::Mutex, sync::Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::info;
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
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Arc::new(Config::from_env()?);
    let pool = PgPoolOptions::new()
        .min_connections(2)
        .max_connections(10)
        .connect(&config.db_dsn)
        .await?;
    ensure_schema(&pool).await?;

    fs::create_dir_all(&config.upload_temp_dir)?;
    if let Some(parent) = config.catalog_state_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let metrics = Arc::new(metrics::AdminMetrics::default());
    let shutdown = CancellationToken::new();
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
    };
    cleanup_expired_approvals(&state).await?;
    recover_interrupted_jobs(state.clone()).await?;
    start_job_workers(&state);
    tokio::spawn(approval_cleanup_loop(state.clone()));

    let app = routes::router(&config, state)?;

    let listener = TcpListener::bind(config.bind_addr).await?;
    info!("rust_admin listening on {}", config.bind_addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown))
        .await?;
    Ok(())
}

async fn shutdown_signal(shutdown: CancellationToken) {
    autvid_common::shutdown_signal().await;
    info!("shutdown signal received");
    shutdown.cancel();
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
