use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    db::{db_error, parse_video_uuid},
    errors::ApiError,
    models::{PublicCatalogDocument, PublicCatalogVariant, PublicCatalogVideo},
    state::AppState,
    CATALOG_CONTENT_TYPE, CATALOG_SCHEMA_VERSION, STATUS_READY,
};

pub(crate) async fn build_catalog_entry_from_db(
    state: &AppState,
    video_id: &str,
    manifest_address: String,
) -> Result<PublicCatalogVideo, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let video_row = sqlx::query(
        r#"
        SELECT title, original_filename, description, created_at, updated_at, is_public,
               show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let variant_rows = sqlx::query(
        r#"
        SELECT resolution, width, height, total_duration, segment_count
        FROM video_variants
        WHERE video_id=$1
        ORDER BY height DESC
        "#,
    )
    .bind(video_uuid)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    Ok(catalog_entry(
        video_id,
        manifest_address,
        &video_row,
        &variant_rows,
    ))
}

fn catalog_entry(
    video_id: &str,
    manifest_address: String,
    video_row: &sqlx::sqlite::SqliteRow,
    variant_rows: &[sqlx::sqlite::SqliteRow],
) -> PublicCatalogVideo {
    PublicCatalogVideo {
        id: video_id.to_string(),
        title: video_row.try_get("title").unwrap_or_default(),
        original_filename: None,
        description: video_row.try_get("description").ok().flatten(),
        status: STATUS_READY.to_string(),
        created_at: video_row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|_| Utc::now().to_rfc3339()),
        updated_at: video_row
            .try_get::<DateTime<Utc>, _>("updated_at")
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|_| Utc::now().to_rfc3339()),
        manifest_address,
        is_public: video_row.try_get::<bool, _>("is_public").unwrap_or(false),
        show_original_filename: false,
        show_manifest_address: video_row
            .try_get::<bool, _>("show_manifest_address")
            .unwrap_or(false),
        variants: variant_rows
            .iter()
            .map(|variant| PublicCatalogVariant {
                resolution: variant
                    .try_get::<String, _>("resolution")
                    .unwrap_or_default(),
                width: variant.try_get::<i32, _>("width").unwrap_or_default(),
                height: variant.try_get::<i32, _>("height").unwrap_or_default(),
                segment_count: variant
                    .try_get::<Option<i32>, _>("segment_count")
                    .ok()
                    .flatten()
                    .unwrap_or(0),
                total_duration: variant
                    .try_get::<Option<f64>, _>("total_duration")
                    .ok()
                    .flatten(),
            })
            .collect(),
    }
}

pub(crate) async fn build_all_catalog_from_db(
    state: &AppState,
) -> Result<PublicCatalogDocument, ApiError> {
    let mut tx = state.pool.begin().await.map_err(db_error)?;
    let catalog = build_catalog_on_connection(&mut tx).await?;
    tx.commit().await.map_err(db_error)?;
    Ok(catalog)
}

pub(crate) async fn build_catalog_on_connection(
    connection: &mut sqlx::SqliteConnection,
) -> Result<PublicCatalogDocument, ApiError> {
    let rows = sqlx::query("SELECT * FROM videos WHERE status=$1 AND manifest_address IS NOT NULL ORDER BY updated_at DESC, created_at DESC, id")
        .bind(STATUS_READY)
        .fetch_all(&mut *connection).await.map_err(db_error)?;
    let variants = sqlx::query("SELECT vv.* FROM video_variants vv JOIN videos v ON v.id=vv.video_id WHERE v.status=$1 AND v.manifest_address IS NOT NULL ORDER BY vv.height DESC, vv.resolution, vv.id")
        .bind(STATUS_READY)
        .fetch_all(&mut *connection).await.map_err(db_error)?;
    let mut by_video = std::collections::HashMap::<Uuid, Vec<sqlx::sqlite::SqliteRow>>::new();
    for row in variants {
        by_video
            .entry(row.try_get("video_id").map_err(db_error)?)
            .or_default()
            .push(row);
    }

    let mut videos = Vec::with_capacity(rows.len());
    for row in rows {
        let video_id: Uuid = row.try_get("id").map_err(db_error)?;
        let Some(manifest_address) = row
            .try_get::<Option<String>, _>("manifest_address")
            .ok()
            .flatten()
        else {
            continue;
        };
        videos.push(catalog_entry(
            &video_id.to_string(),
            manifest_address,
            &row,
            &by_video.remove(&video_id).unwrap_or_default(),
        ));
    }

    let generated_at = Utc::now().to_rfc3339();
    Ok(PublicCatalogDocument {
        schema_version: CATALOG_SCHEMA_VERSION,
        content_type: CATALOG_CONTENT_TYPE.to_string(),
        catalog_kind: "all".to_string(),
        generated_at: generated_at.clone(),
        updated_at: generated_at,
        videos,
    })
}
