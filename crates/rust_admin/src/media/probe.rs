use std::path::Path as FsPath;

use axum::http::StatusCode;
use serde_json::Value;
use tokio::process::Command;

use crate::{
    config::duration_from_secs_f64,
    errors::ApiError,
    models::{CommandOutput, UploadMediaMetadata},
    state::AppState,
};

use super::*;

pub(crate) async fn run_command_output(
    mut command: Command,
    timeout_seconds: Option<f64>,
) -> Result<CommandOutput, ApiError> {
    let child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not start media tool: {err}"),
            )
        })?;

    let wait = child.wait_with_output();
    let output = if let Some(seconds) = timeout_seconds {
        tokio::time::timeout(duration_from_secs_f64(seconds), wait)
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "Media tool exceeded the configured runtime limit",
                )
            })?
    } else {
        wait.await
    }
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Media tool failed to run: {err}"),
        )
    })?;

    Ok(CommandOutput {
        status_code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

pub(crate) async fn probe_upload_media(
    state: &AppState,
    src: &FsPath,
) -> Result<UploadMediaMetadata, ApiError> {
    let src = assert_under(src, &state.config.upload_temp_dir)?;
    let mut command = Command::new(&state.config.ffprobe_bin);
    command
        .arg("-v")
        .arg("error")
        .arg("-show_streams")
        .arg("-show_format")
        .arg("-of")
        .arg("json")
        .arg(&src);
    let output =
        run_command_output(command, Some(state.config.upload_ffprobe_timeout_seconds)).await?;
    if output.status_code != Some(0) {
        let detail = stderr_tail(&output.stderr, 500);
        let message = if detail.is_empty() {
            "Uploaded file is not a readable video".to_string()
        } else {
            format!("Uploaded file is not a readable video: {detail}")
        };
        return Err(ApiError::new(StatusCode::BAD_REQUEST, message));
    }

    let data: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "Uploaded file probe returned invalid metadata",
        )
    })?;
    let stream = data
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| {
            streams
                .iter()
                .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"))
        })
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "Uploaded file does not contain a video stream",
            )
        })?;

    let width = stream.get("width").and_then(Value::as_i64).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "Uploaded video stream has no usable dimensions",
        )
    })? as i32;
    let height = stream
        .get("height")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "Uploaded video stream has no usable dimensions",
            )
        })? as i32;
    if width <= 0 || height <= 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Uploaded video stream has invalid dimensions",
        ));
    }
    let dimensions =
        if stream_rotation_degrees(stream) == 90 || stream_rotation_degrees(stream) == 270 {
            (height, width)
        } else {
            (width, height)
        };
    let duration_seconds = parse_probe_duration(&data, stream).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "Uploaded video has no usable duration",
        )
    })?;
    enforce_upload_media_limits(state, duration_seconds, dimensions.0, dimensions.1)?;
    Ok(UploadMediaMetadata {
        duration_seconds,
        dimensions,
    })
}

pub(crate) async fn probe_duration(
    state: &AppState,
    src: &FsPath,
) -> Result<Option<f64>, ApiError> {
    let src = assert_under(src, &state.config.upload_temp_dir)?;
    let mut command = Command::new(&state.config.ffprobe_bin);
    command
        .arg("-v")
        .arg("quiet")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("default=noprint_wrappers=1:nokey=1")
        .arg(&src);
    let output = run_command_output(command, None).await?;
    if output.status_code != Some(0) {
        return Ok(None);
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    Ok(raw
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0))
}

pub(crate) async fn probe_video_dimensions(
    state: &AppState,
    src: &FsPath,
) -> Result<Option<(i32, i32)>, ApiError> {
    let src = assert_under(src, &state.config.upload_temp_dir)?;
    let mut command = Command::new(&state.config.ffprobe_bin);
    command
        .arg("-v")
        .arg("quiet")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-show_streams")
        .arg("-of")
        .arg("json")
        .arg(&src);
    let output = run_command_output(command, None).await?;
    if output.status_code != Some(0) {
        return Ok(None);
    }
    let Ok(data) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Ok(None);
    };
    let Some(stream) = data
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| streams.first())
    else {
        return Ok(None);
    };
    let Some(width) = stream
        .get("width")
        .and_then(Value::as_i64)
        .map(|value| value as i32)
    else {
        return Ok(None);
    };
    let Some(height) = stream
        .get("height")
        .and_then(Value::as_i64)
        .map(|value| value as i32)
    else {
        return Ok(None);
    };
    if stream_rotation_degrees(stream) == 90 || stream_rotation_degrees(stream) == 270 {
        Ok(Some((height, width)))
    } else {
        Ok(Some((width, height)))
    }
}

pub(crate) fn stream_rotation_degrees(stream: &Value) -> i32 {
    let rotation = stream
        .get("tags")
        .and_then(|tags| tags.get("rotate"))
        .and_then(value_to_i32)
        .or_else(|| {
            stream
                .get("side_data_list")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find_map(|item| item.get("rotation").and_then(value_to_i32))
                })
        })
        .unwrap_or(0);
    rotation.rem_euclid(360)
}

pub(crate) fn value_to_i32(value: &Value) -> Option<i32> {
    value
        .as_i64()
        .map(|value| value as i32)
        .or_else(|| value.as_f64().map(|value| value as i32))
        .or_else(|| {
            value
                .as_str()?
                .parse::<f64>()
                .ok()
                .map(|value| value as i32)
        })
}

pub(crate) fn parse_probe_duration(data: &Value, stream: &Value) -> Option<f64> {
    [stream, data.get("format").unwrap_or(&Value::Null)]
        .into_iter()
        .filter_map(|source| {
            source
                .get("duration")
                .and_then(|value| {
                    value
                        .as_f64()
                        .or_else(|| value.as_str()?.parse::<f64>().ok())
                })
                .filter(|value| value.is_finite() && *value > 0.0)
        })
        .next()
}
