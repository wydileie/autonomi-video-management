use std::{fs, time::Duration as StdDuration};

use sqlx::Row;
use tokio::time::sleep;
use tracing::{info, warn};
use uuid::Uuid;

use crate::state::AppState;

pub(crate) async fn cleanup_expired_approvals(state: &AppState) -> anyhow::Result<()> {
    let rows = sqlx::query(
        r#"
        UPDATE videos
        SET status='expired',
            error_message='Final quote approval window expired; local files were deleted.',
            updated_at=NOW()
        WHERE status='awaiting_approval'
          AND approval_expires_at IS NOT NULL
          AND approval_expires_at <= NOW()
        RETURNING id, job_dir
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    for row in rows {
        if let Ok(Some(job_dir)) = row.try_get::<Option<String>, _>("job_dir") {
            let _ = fs::remove_dir_all(job_dir);
        }
        if let Ok(video_id) = row.try_get::<Uuid, _>("id") {
            info!(
                "Expired awaiting approval video {} and removed local files",
                video_id
            );
        }
    }
    Ok(())
}

pub(crate) async fn approval_cleanup_loop(state: AppState) {
    loop {
        sleep(StdDuration::from_secs(
            state.config.approval_cleanup_interval_seconds,
        ))
        .await;
        if let Err(err) = cleanup_expired_approvals(&state).await {
            warn!("Approval cleanup failed: {}", err);
        }
    }
}
