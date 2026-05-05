use std::{
    fs,
    path::{Path as FsPath, PathBuf},
};

use axum::{
    extract::Multipart,
    http::{header, HeaderMap, StatusCode},
};
use serde_json::json;
use tokio::{fs as tokio_fs, io::AsyncWriteExt};
use tracing::info;
use uuid::Uuid;

use crate::{
    catalog::get_db_video,
    db::db_error,
    errors::ApiError,
    media::{
        enforce_upload_media_limits, probe_upload_media, resolution_preset,
        supported_resolutions_error,
    },
    models::{AcceptedUpload, UploadMediaMetadata},
    state::AppState,
};

struct JobDirGuard {
    path: PathBuf,
    armed: bool,
}

impl JobDirGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for JobDirGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub(crate) async fn accept_upload(
    state: &AppState,
    headers: &HeaderMap,
    mut multipart: Multipart,
    username: &str,
) -> Result<AcceptedUpload, ApiError> {
    let multipart_overhead_allowance = 2 * 1024 * 1024_u64;
    if let Some(content_length) = content_length(headers) {
        if content_length > state.config.upload_max_file_bytes + multipart_overhead_allowance {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "Upload exceeds max file size ({})",
                    format_bytes(state.config.upload_max_file_bytes)
                ),
            ));
        }
    }

    let _permit = state
        .upload_save_semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many uploads are in progress; try again shortly",
            )
        })?;

    let video_uuid = Uuid::new_v4();
    let video_id = video_uuid.to_string();
    let job_dir = state.config.upload_temp_dir.join(&video_id);
    fs::create_dir_all(&job_dir).map_err(|err| {
        ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!("Could not create upload directory: {err}"),
        )
    })?;
    let guard = JobDirGuard::new(job_dir.clone());

    if let Some(content_length) = content_length(headers) {
        ensure_upload_disk_space(state, content_length)?;
    } else {
        ensure_upload_disk_space(state, 0)?;
    }

    let mut title: Option<String> = None;
    let mut description = String::new();
    let mut resolutions = String::from("720p");
    let mut show_manifest_address = false;
    let mut upload_original = false;
    let mut publish_when_ready = false;
    let mut original_filename: Option<String> = None;
    let mut source_path: Option<PathBuf> = None;
    let mut upload_metadata: Option<UploadMediaMetadata> = None;

    while let Some(mut field) = multipart.next_field().await.map_err(|err| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("Invalid multipart upload: {err}"),
        )
    })? {
        let Some(name) = field.name().map(str::to_string) else {
            continue;
        };

        if name == "file" {
            if source_path.is_some() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "Only one upload file is supported",
                ));
            }
            let safe_filename = sanitize_upload_filename(field.file_name());
            let src_path = job_dir.join(format!("original_{safe_filename}"));
            let tmp_src_path = src_path.with_file_name(format!(
                "{}.uploading",
                src_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("upload")
            ));
            let mut output = tokio_fs::File::create(&tmp_src_path).await.map_err(|err| {
                ApiError::new(
                    StatusCode::INSUFFICIENT_STORAGE,
                    format!("Could not store upload safely: {err}"),
                )
            })?;
            let mut bytes_written = 0_u64;
            while let Some(chunk) = field.chunk().await.map_err(|err| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("Could not read upload: {err}"),
                )
            })? {
                let next_size = bytes_written + chunk.len() as u64;
                if next_size > state.config.upload_max_file_bytes {
                    let _ = tokio_fs::remove_file(&tmp_src_path).await;
                    return Err(ApiError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!(
                            "Upload exceeds max file size ({})",
                            format_bytes(state.config.upload_max_file_bytes)
                        ),
                    ));
                }
                ensure_upload_disk_space(state, chunk.len() as u64)?;
                output.write_all(&chunk).await.map_err(|err| {
                    ApiError::new(
                        StatusCode::INSUFFICIENT_STORAGE,
                        format!("Could not store upload safely: {err}"),
                    )
                })?;
                bytes_written = next_size;
            }
            output.flush().await.map_err(|err| {
                ApiError::new(
                    StatusCode::INSUFFICIENT_STORAGE,
                    format!("Could not store upload safely: {err}"),
                )
            })?;
            drop(output);
            if bytes_written == 0 {
                let _ = tokio_fs::remove_file(&tmp_src_path).await;
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "Uploaded file is empty",
                ));
            }

            let metadata = probe_upload_media(state, &tmp_src_path).await?;
            tokio_fs::rename(&tmp_src_path, &src_path)
                .await
                .map_err(|err| {
                    ApiError::new(
                        StatusCode::INSUFFICIENT_STORAGE,
                        format!("Could not store upload safely: {err}"),
                    )
                })?;
            info!(
                "Accepted upload {} filename={} bytes={} duration={:.2}s dimensions={}x{}",
                video_id,
                safe_filename,
                bytes_written,
                metadata.duration_seconds,
                metadata.dimensions.0,
                metadata.dimensions.1
            );
            original_filename = Some(safe_filename);
            source_path = Some(src_path);
            upload_metadata = Some(metadata);
        } else {
            let text = field.text().await.map_err(|err| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("Invalid form field {name}: {err}"),
                )
            })?;
            match name.as_str() {
                "title" => title = Some(text.trim().to_string()),
                "description" => description = text.trim().to_string(),
                "resolutions" => resolutions = text,
                "show_original_filename" => {}
                "show_manifest_address" => show_manifest_address = parse_form_bool(&text),
                "upload_original" => upload_original = parse_form_bool(&text),
                "publish_when_ready" => publish_when_ready = parse_form_bool(&text),
                _ => {}
            }
        }
    }

    let title = title
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "title is required"))?;
    let original_filename = original_filename
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "file is required"))?;
    let source_path =
        source_path.ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "file is required"))?;
    let selected = parse_resolutions(&resolutions);
    if selected.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            supported_resolutions_error(),
        ));
    }
    if let Some(metadata) = upload_metadata {
        enforce_upload_media_limits(
            state,
            metadata.duration_seconds,
            metadata.dimensions.0,
            metadata.dimensions.1,
        )?;
    }

    sqlx::query(
        r#"
        INSERT INTO videos (
            id, title, original_filename, description, status, job_dir,
            job_source_path, requested_resolutions,
            show_original_filename, show_manifest_address,
            upload_original, publish_when_ready, user_id
        )
        VALUES ($1, $2, $3, $4, 'pending', $5, $6, $7::jsonb, $8, $9, $10, $11, $12)
        "#,
    )
    .bind(video_uuid)
    .bind(&title)
    .bind(&original_filename)
    .bind(if description.is_empty() {
        None
    } else {
        Some(description.as_str())
    })
    .bind(job_dir.to_string_lossy().as_ref())
    .bind(source_path.to_string_lossy().as_ref())
    .bind(json!(selected))
    .bind(false)
    .bind(show_manifest_address)
    .bind(upload_original)
    .bind(publish_when_ready)
    .bind(username)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    let video = get_db_video(state, &video_id, false).await?;
    guard.disarm();
    Ok(AcceptedUpload { video_id, video })
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

fn parse_form_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub(crate) fn parse_resolutions(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|resolution| resolution_preset(resolution).is_some())
        .map(str::to_string)
        .collect()
}

pub(crate) fn sanitize_upload_filename(filename: Option<&str>) -> String {
    let basename = filename
        .and_then(|name| FsPath::new(name).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("upload");
    let path = FsPath::new(basename);
    let raw_stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("upload");
    let raw_suffix = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let mut safe_stem: String = raw_stem
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(&['.', '_', '-'][..])
        .to_string();
    if safe_stem.is_empty() {
        safe_stem = "upload".to_string();
    }
    let mut safe_suffix: String = raw_suffix
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(15)
        .collect::<String>()
        .to_ascii_lowercase();
    if !safe_suffix.is_empty() {
        safe_suffix.insert(0, '.');
    }
    let max_stem_length = 128_usize.saturating_sub(safe_suffix.len()).max(1);
    if safe_stem.len() > max_stem_length {
        safe_stem.truncate(max_stem_length);
    }
    format!("{safe_stem}{safe_suffix}")
}

fn ensure_upload_disk_space(state: &AppState, additional_bytes: u64) -> Result<(), ApiError> {
    let free_bytes = fs2::available_space(&state.config.upload_temp_dir).map_err(|err| {
        ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!("Could not inspect upload disk space: {err}"),
        )
    })?;
    let required_free = state
        .config
        .upload_min_free_bytes
        .saturating_add(additional_bytes);
    if free_bytes < required_free {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!(
                "Not enough upload disk space (free={}, required={})",
                format_bytes(free_bytes),
                format_bytes(required_free)
            ),
        ));
    }
    Ok(())
}

pub(crate) fn format_bytes(byte_count: u64) -> String {
    let mut value = byte_count as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 || unit == "TiB" {
            if unit == "B" {
                return format!("{byte_count} B");
            }
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{byte_count} B")
}

#[cfg(test)]
mod tests {
    use axum::http::{header, HeaderMap, HeaderValue};

    use super::{content_length, parse_form_bool, parse_resolutions, sanitize_upload_filename};

    #[test]
    fn sanitizes_upload_filename_like_admin_service() {
        assert_eq!(
            sanitize_upload_filename(Some("../My Video!!.MP4")),
            "My_Video.mp4"
        );
        assert_eq!(sanitize_upload_filename(Some("...")), "upload");
    }

    #[test]
    fn parses_only_supported_resolutions() {
        assert_eq!(
            parse_resolutions("720p, nope,1440p,1080p,4k"),
            vec!["720p", "1440p", "1080p", "4k"]
        );
    }

    #[test]
    fn parses_form_booleans_from_common_browser_values() {
        for truthy in ["1", "true", "TRUE", "yes", "on", " On "] {
            assert!(parse_form_bool(truthy), "{truthy} should be truthy");
        }
        for falsy in ["0", "false", "no", "off", "", "maybe"] {
            assert!(!parse_form_bool(falsy), "{falsy} should be falsy");
        }
    }

    #[test]
    fn parses_content_length_only_when_valid() {
        let mut headers = HeaderMap::new();
        assert_eq!(content_length(&headers), None);

        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("42"));
        assert_eq!(content_length(&headers), Some(42));

        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_static("not-a-number"),
        );
        assert_eq!(content_length(&headers), None);
    }
}
