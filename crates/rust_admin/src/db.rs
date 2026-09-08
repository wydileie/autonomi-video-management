use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::{errors::ApiError, state::AppState};

pub async fn ensure_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    let mut connection = pool.acquire().await?;
    // A dedicated connection keeps the table-replacement pragma away from
    // normal requests and preserves children while expanding CHECK constraints.
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut *connection)
        .await?;
    let result = sqlx::migrate!("./migrations").run(&mut *connection).await;
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut *connection)
        .await;
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *connection)
        .await?;
    result?;
    anyhow::ensure!(
        violations?.is_empty(),
        "Migration left invalid foreign keys"
    );
    Ok(())
}

pub(crate) async fn begin_immediate(
    pool: &SqlitePool,
) -> Result<Transaction<'_, Sqlite>, ApiError> {
    pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_error)
}

/// Hold SQLite's writer lock from ownership validation through the side effect.
pub(crate) async fn begin_fenced(state: &AppState) -> Result<Transaction<'_, Sqlite>, ApiError> {
    let mut tx = begin_immediate(&state.pool).await?;
    if let Some(lease) = &state.active_job {
        let owns: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM video_jobs WHERE id=$1 AND lease_owner=$2 AND attempts=$3 AND status='running' AND lease_expires_at > $4)")
            .bind(lease.id).bind(&lease.owner).bind(lease.generation).bind(Utc::now())
            .fetch_one(&mut *tx).await.map_err(db_error)?;
        if !owns {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "Job lease ownership lost",
            ));
        }
    }
    Ok(tx)
}

pub(crate) async fn execute_fenced<'q>(
    state: &AppState,
    query: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments>,
) -> Result<sqlx::sqlite::SqliteQueryResult, ApiError> {
    let mut tx = begin_fenced(state).await?;
    let result = query.execute(&mut *tx).await.map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(result)
}

pub(crate) async fn set_status(
    state: &AppState,
    video_id: &str,
    status: &str,
    error_message: Option<&str>,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let now = Utc::now();
    crate::db::execute_fenced(
        state,
        sqlx::query(
            r#"
        UPDATE videos
        SET status=$1, error_message=$2, updated_at=$3
        WHERE id=$4
        "#,
        )
        .bind(status)
        .bind(error_message)
        .bind(now)
        .bind(video_uuid),
    )
    .await?;
    Ok(())
}

pub(crate) async fn set_awaiting_approval(
    state: &AppState,
    video_id: &str,
    final_quote: Value,
    expires_at: DateTime<Utc>,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let now = Utc::now();
    crate::db::execute_fenced(
        state,
        sqlx::query(
            r#"
        UPDATE videos
        SET status='awaiting_approval',
            final_quote=$1,
            final_quote_created_at=$2,
            approval_expires_at=$3,
            error_message=NULL,
            updated_at=$2
        WHERE id=$4
        "#,
        )
        .bind(final_quote)
        .bind(now)
        .bind(expires_at)
        .bind(video_uuid),
    )
    .await?;
    Ok(())
}

pub(crate) fn db_error(err: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

pub(crate) fn parse_video_uuid(video_id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(video_id).map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))
}

#[cfg(all(test, feature = "db-tests"))]
mod db_migration_tests {
    use super::*;

    #[tokio::test]
    async fn db_upgrade_preserves_children_and_expands_recovery_contracts() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("database");
        sqlx::migrate!("./migrations")
            .run_to(2, &pool)
            .await
            .expect("legacy schema");
        sqlx::raw_sql("INSERT INTO videos(id,title,original_filename,status,encode_settings) VALUES('video','title','source','awaiting_approval','{}');
            INSERT INTO video_variants(id,video_id,resolution,width,height,video_bitrate,audio_bitrate) VALUES('variant','video','360p',640,360,1,1);
            INSERT INTO video_segments(id,variant_id,segment_index,autonomi_address) VALUES('segment','variant',0,'published-address');
            INSERT INTO video_jobs(id,job_kind,video_id) VALUES('job','upload_video','video');")
            .execute(&pool).await.expect("legacy rows");
        ensure_schema(&pool).await.expect("upgrade");
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM videos")
                .fetch_one(&pool)
                .await
                .expect("status"),
            "approval_required"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT autonomi_address FROM video_segments")
                .fetch_one(&pool)
                .await
                .expect("segment"),
            "published-address"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM video_jobs")
                .fetch_one(&pool)
                .await
                .expect("jobs"),
            1
        );
        sqlx::raw_sql("UPDATE videos SET status='payment_recovery_required'; UPDATE video_jobs SET job_kind='quote_video'; UPDATE video_jobs SET job_kind='finalize_catalog';")
            .execute(&pool).await.expect("new states");
        assert!(sqlx::query("UPDATE videos SET status='invalid'")
            .execute(&pool)
            .await
            .is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
                .fetch_one(&pool)
                .await
                .expect("foreign keys"),
            1
        );
        sqlx::query("DELETE FROM videos")
            .execute(&pool)
            .await
            .expect("delete");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM video_segments")
                .fetch_one(&pool)
                .await
                .expect("cascade"),
            0
        );
    }
}
