use std::{
    fs,
    path::{Path as FsPath, PathBuf},
};

use serde_json::Value;
use sqlx::Row;
use tracing::{info, warn};
use uuid::Uuid;

use super::scheduling::{
    enqueue_catalog_publish_job, schedule_processing_job, schedule_upload_job,
};
use crate::{
    catalog::read_catalog_state_value, db::set_status, media::resolution_preset, state::AppState,
    upload::parse_resolutions, JOB_STATUS_QUEUED, JOB_STATUS_RUNNING, STATUS_ERROR, STATUS_PENDING,
    STATUS_PROCESSING, STATUS_UPLOADING,
};

pub(crate) async fn recover_interrupted_jobs(state: AppState) -> anyhow::Result<()> {
    let reset_jobs = sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            lease_owner=NULL,
            lease_expires_at=NULL,
            run_after=NOW(),
            updated_at=NOW()
        WHERE status=$2
        "#,
    )
    .bind(JOB_STATUS_QUEUED)
    .bind(JOB_STATUS_RUNNING)
    .execute(&state.pool)
    .await?;
    if reset_jobs.rows_affected() > 0 {
        info!(
            "Requeued {} running durable job(s) from a previous admin process",
            reset_jobs.rows_affected()
        );
    }

    let rows = sqlx::query(
        r#"
        SELECT id, status, job_dir, job_source_path, requested_resolutions
        FROM videos
        WHERE status IN ('pending', 'processing', 'uploading')
        ORDER BY created_at
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let mut recovered_processing = 0;
    let mut recovered_uploads = 0;
    for row in rows {
        let video_id: Uuid = row.try_get("id")?;
        let video_id = video_id.to_string();
        let status: String = row.try_get("status")?;
        let job_dir = row
            .try_get::<Option<String>, _>("job_dir")?
            .map(PathBuf::from);

        if matches!(status.as_str(), STATUS_PENDING | STATUS_PROCESSING) {
            let resolutions = decode_requested_resolutions(
                row.try_get::<Option<Value>, _>("requested_resolutions")?,
            );
            let source_path = recover_source_path(
                job_dir.as_deref(),
                row.try_get::<Option<String>, _>("job_source_path")?
                    .as_deref(),
            );
            if job_dir.as_ref().is_none_or(|path| !path.exists())
                || source_path.is_none()
                || resolutions.is_empty()
            {
                let _ = set_status(
                    &state,
                    &video_id,
                    STATUS_ERROR,
                    Some(
                        "Interrupted processing job could not be recovered because its source file or requested resolutions were missing.",
                    ),
                )
                .await;
                warn!("Could not recover interrupted processing job {}", video_id);
                continue;
            }

            schedule_processing_job(&state, &video_id)
                .await
                .map_err(|err| anyhow::anyhow!(err.detail))?;
            recovered_processing += 1;
        } else if status == STATUS_UPLOADING {
            schedule_upload_job(&state, &video_id)
                .await
                .map_err(|err| anyhow::anyhow!(err.detail))?;
            recovered_uploads += 1;
        }
    }

    if recovered_processing > 0 || recovered_uploads > 0 {
        info!(
            "Recovered interrupted jobs: processing={} uploading={}",
            recovered_processing, recovered_uploads
        );
    }
    if read_catalog_state_value(&state.config)
        .and_then(|value| value.get("publish_pending").and_then(Value::as_bool))
        .unwrap_or(false)
    {
        enqueue_catalog_publish_job(&state)
            .await
            .map_err(|err| anyhow::anyhow!(err.detail))?;
        info!("Recovered pending catalog publish from local catalog state");
    }
    Ok(())
}

pub(super) fn decode_requested_resolutions(value: Option<Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .filter(|item| resolution_preset(item).is_some())
            .collect(),
        Some(Value::String(value)) => parse_resolutions(&value),
        _ => vec![],
    }
}

pub(super) fn recover_source_path(
    job_dir: Option<&FsPath>,
    job_source_path: Option<&str>,
) -> Option<PathBuf> {
    if let Some(source_path) = job_source_path
        .map(PathBuf::from)
        .filter(|path| path.exists())
    {
        return Some(source_path);
    }

    let job_dir = job_dir?;
    let mut matches = fs::read_dir(job_dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("original_"))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.into_iter().next()
}
