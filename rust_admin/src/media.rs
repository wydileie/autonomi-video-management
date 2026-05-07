use std::{
    fs, io,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Instant,
};

use axum::http::StatusCode;
use serde_json::Value;
use tokio::{process::Command, sync::Semaphore, task::JoinSet};
use tracing::{info, instrument};

use crate::{
    config::duration_from_secs_f64,
    errors::ApiError,
    models::{CommandOutput, TranscodedRendition, TranscodedSegment, UploadMediaMetadata},
    state::AppState,
    MIN_ANTD_SELF_ENCRYPTION_BYTES, SUPPORTED_RESOLUTIONS,
};

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
    let mut command = Command::new("ffprobe");
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
    let mut command = Command::new("ffprobe");
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
    let mut command = Command::new("ffprobe");
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

async fn run_ffmpeg(
    state: &AppState,
    src: &FsPath,
    seg_dir: &FsPath,
    width: i32,
    height: i32,
    video_kbps: i32,
    audio_kbps: i32,
) -> Result<(), ApiError> {
    let src = assert_under(src, &state.config.upload_temp_dir)?;
    let seg_dir = assert_under(seg_dir, &state.config.upload_temp_dir)?;
    let segment_pattern =
        assert_under(&seg_dir.join("seg_%05d.ts"), &state.config.upload_temp_dir)?;
    let segment_time = format!("{}", F64Format(state.config.hls_segment_duration));
    let mut command = Command::new("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-nostats")
        .arg("-loglevel")
        .arg("warning")
        .arg("-y")
        .arg("-filter_threads")
        .arg(state.config.ffmpeg_filter_threads.to_string())
        .arg("-i")
        .arg(&src)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a?")
        .arg("-sn")
        .arg("-c:v")
        .arg("libx264")
        .arg("-threads")
        .arg(state.config.ffmpeg_threads.to_string())
        .arg("-preset")
        .arg("veryfast")
        .arg("-profile:v")
        .arg("high")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-vf")
        .arg(format!(
            "scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"
        ))
        .arg("-b:v")
        .arg(format!("{video_kbps}k"))
        .arg("-maxrate")
        .arg(format!("{}k", video_kbps * 3 / 2))
        .arg("-bufsize")
        .arg(format!("{}k", video_kbps * 2))
        .arg("-force_key_frames")
        .arg(format!("expr:gte(t,n_forced*{segment_time})"))
        .arg("-sc_threshold")
        .arg("0")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg(format!("{audio_kbps}k"))
        .arg("-ar")
        .arg("44100")
        .arg("-f")
        .arg("segment")
        .arg("-segment_time")
        .arg(segment_time)
        .arg("-segment_time_delta")
        .arg("0.05")
        .arg("-segment_format")
        .arg("mpegts")
        .arg("-reset_timestamps")
        .arg("1")
        .arg(segment_pattern);
    let output =
        run_command_output(command, Some(state.config.ffmpeg_rendition_timeout_seconds)).await?;
    if output.status_code != Some(0) {
        let mut detail = stderr_tail(&output.stderr, 2000);
        if output.status_code == Some(137) {
            detail = format!(
                "FFmpeg was killed, which usually means the container ran out of memory while transcoding. FFMPEG_THREADS={}, FFMPEG_FILTER_THREADS={}. {detail}",
                state.config.ffmpeg_threads, state.config.ffmpeg_filter_threads
            );
        }
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "FFmpeg failed with exit code {:?}: {detail}",
                output.status_code
            ),
        ));
    }
    Ok(())
}

struct F64Format(f64);

impl std::fmt::Display for F64Format {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = if (self.0.fract()).abs() < f64::EPSILON {
            format!("{}", self.0 as i64)
        } else {
            let mut text = format!("{:.6}", self.0);
            while text.contains('.') && text.ends_with('0') {
                text.pop();
            }
            text
        };
        formatter.write_str(&text)
    }
}

fn stderr_tail(stderr: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(stderr);
    let start = text.len().saturating_sub(limit);
    text[start..].trim().to_string()
}

pub(crate) fn assert_under(path: &FsPath, root: &FsPath) -> Result<PathBuf, ApiError> {
    let root = fs::canonicalize(root).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not inspect upload temp directory: {err}"),
        )
    })?;

    let target = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Media path has no parent directory",
                )
            })?;
            let parent = fs::canonicalize(parent).map_err(|err| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Could not inspect media path parent: {err}"),
                )
            })?;
            let file_name = path.file_name().ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Media path has no file name",
                )
            })?;
            parent.join(file_name)
        }
        Err(err) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not inspect media path: {err}"),
            ));
        }
    };

    if !target.starts_with(&root) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Media path is outside the configured upload workspace",
        ));
    }

    Ok(target)
}

fn stream_rotation_degrees(stream: &Value) -> i32 {
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

fn value_to_i32(value: &Value) -> Option<i32> {
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

fn parse_probe_duration(data: &Value, stream: &Value) -> Option<f64> {
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

#[instrument(
    skip(state, source_path, resolutions, job_dir),
    fields(video_id = %video_id, resolution_count = resolutions.len())
)]
pub(crate) async fn transcode_renditions(
    state: &AppState,
    video_id: &str,
    source_path: &FsPath,
    resolutions: &[String],
    job_dir: &FsPath,
    source_dimensions: Option<(i32, i32)>,
) -> Result<Vec<TranscodedRendition>, ApiError> {
    let semaphore = Arc::new(Semaphore::new(state.config.ffmpeg_max_parallel_renditions));
    let mut jobs = JoinSet::new();
    let mut scheduled = 0_usize;

    for (order, resolution) in resolutions.iter().enumerate() {
        if resolution_preset(resolution).is_none() {
            continue;
        }
        let state = state.clone();
        let semaphore = semaphore.clone();
        let video_id = video_id.to_string();
        let source_path = source_path.to_path_buf();
        let job_dir = job_dir.to_path_buf();
        let resolution = resolution.clone();
        scheduled += 1;
        jobs.spawn(async move {
            let _permit = semaphore.acquire_owned().await.map_err(|err| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Could not acquire FFmpeg rendition slot: {err}"),
                )
            })?;
            transcode_one_rendition(
                &state,
                &video_id,
                &source_path,
                &job_dir,
                order,
                resolution,
                source_dimensions,
            )
            .await
        });
    }

    if scheduled == 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            supported_resolutions_error(),
        ));
    }

    info!(
        "Transcoding {} rendition(s) for {} with max_parallel={}",
        scheduled, video_id, state.config.ffmpeg_max_parallel_renditions
    );

    let mut renditions = Vec::with_capacity(scheduled);
    while let Some(joined) = jobs.join_next().await {
        match joined {
            Ok(Ok(rendition)) => renditions.push(rendition),
            Ok(Err(err)) => {
                jobs.abort_all();
                return Err(err);
            }
            Err(err) => {
                jobs.abort_all();
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Transcode task failed: {err}"),
                ));
            }
        }
    }
    renditions.sort_by_key(|rendition| rendition.order);
    Ok(renditions)
}

#[instrument(
    skip(state, source_path, job_dir),
    fields(video_id = %video_id, resolution = %resolution, order = order)
)]
async fn transcode_one_rendition(
    state: &AppState,
    video_id: &str,
    source_path: &FsPath,
    job_dir: &FsPath,
    order: usize,
    resolution: String,
    source_dimensions: Option<(i32, i32)>,
) -> Result<TranscodedRendition, ApiError> {
    let Some((preset_width, preset_height, video_kbps, audio_kbps)) =
        resolution_preset(&resolution)
    else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            supported_resolutions_error(),
        ));
    };
    let (width, height) =
        target_dimensions_for_source(preset_width, preset_height, source_dimensions);
    let video_kbps =
        target_video_bitrate_kbps(video_kbps, preset_width, preset_height, width, height);
    let seg_dir = job_dir.join(&resolution);
    fs::create_dir_all(&seg_dir).map_err(|err| {
        ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!("Could not create segment directory: {err}"),
        )
    })?;
    info!("Transcoding {} -> {}", video_id, resolution);
    let ffmpeg_started = Instant::now();
    let ffmpeg_result = run_ffmpeg(
        state,
        source_path,
        &seg_dir,
        width,
        height,
        video_kbps,
        audio_kbps,
    )
    .await;
    state
        .metrics
        .record_ffmpeg_duration(&resolution, ffmpeg_started.elapsed());
    ffmpeg_result?;

    let ts_files = collect_segment_files(&seg_dir)?;
    if ts_files.is_empty() {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("FFmpeg produced no segments for {resolution}"),
        ));
    }

    let mut segments = Vec::with_capacity(ts_files.len());
    for (idx, ts_path) in ts_files.into_iter().enumerate() {
        let duration = probe_duration(state, &ts_path)
            .await?
            .unwrap_or(state.config.hls_segment_duration);
        let byte_size = fs::metadata(&ts_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if byte_size < MIN_ANTD_SELF_ENCRYPTION_BYTES as u64 {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "FFmpeg produced a segment too small for Autonomi storage: {} ({} bytes)",
                    ts_path.display(),
                    byte_size
                ),
            ));
        }
        segments.push(TranscodedSegment {
            segment_index: idx as i32,
            duration,
            byte_size: byte_size as i64,
            local_path: ts_path,
        });
    }

    info!(
        "Transcoded {} -> {} ({} segment(s))",
        video_id,
        resolution,
        segments.len()
    );

    Ok(TranscodedRendition {
        order,
        resolution,
        width,
        height,
        video_kbps,
        audio_kbps,
        segments,
    })
}

fn collect_segment_files(seg_dir: &FsPath) -> Result<Vec<PathBuf>, ApiError> {
    let mut files = fs::read_dir(seg_dir)
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("ts"))
        .filter(|path| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|stem| stem.starts_with("seg_"))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|path| segment_index_from_path(path).unwrap_or(i32::MAX));
    Ok(files)
}

fn segment_index_from_path(path: &FsPath) -> Option<i32> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .and_then(|stem| stem.strip_prefix("seg_"))
        .and_then(|value| value.parse::<i32>().ok())
}
pub(crate) fn resolution_preset(resolution: &str) -> Option<(i32, i32, i32, i32)> {
    match resolution {
        "8k" => Some((7680, 4320, 45000, 320)),
        "4k" => Some((3840, 2160, 16000, 256)),
        "1440p" => Some((2560, 1440, 8000, 192)),
        "1080p" => Some((1920, 1080, 5000, 192)),
        "720p" => Some((1280, 720, 2500, 128)),
        "540p" => Some((960, 540, 1600, 128)),
        "480p" => Some((854, 480, 1000, 128)),
        "360p" => Some((640, 360, 500, 96)),
        "240p" => Some((426, 240, 300, 64)),
        "144p" => Some((256, 144, 150, 48)),
        _ => None,
    }
}

pub(crate) fn supported_resolutions_error() -> String {
    let values = SUPPORTED_RESOLUTIONS
        .iter()
        .map(|resolution| format!("'{resolution}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("No valid resolutions. Choose from: [{values}]")
}

fn even_floor(value: f64) -> i32 {
    let floored = value.floor().max(2.0) as i32;
    let even = floored - floored.rem_euclid(2);
    even.max(2)
}

fn fit_within_source(width: i32, height: i32, source_width: i32, source_height: i32) -> (i32, i32) {
    if width <= source_width && height <= source_height {
        return (width, height);
    }
    let scale = (f64::from(source_width) / f64::from(width))
        .min(f64::from(source_height) / f64::from(height))
        .min(1.0);
    (
        even_floor(f64::from(width) * scale),
        even_floor(f64::from(height) * scale),
    )
}

pub(crate) fn target_dimensions_for_source(
    preset_width: i32,
    preset_height: i32,
    source_dimensions: Option<(i32, i32)>,
) -> (i32, i32) {
    let short_edge = preset_width.min(preset_height);
    let Some((source_width, source_height)) = source_dimensions else {
        return (preset_width, preset_height);
    };
    if source_height > source_width {
        let width = short_edge;
        let height =
            even_floor(f64::from(short_edge) * f64::from(source_height) / f64::from(source_width));
        fit_within_source(width, height, source_width, source_height)
    } else if source_width > source_height {
        let width =
            even_floor(f64::from(short_edge) * f64::from(source_width) / f64::from(source_height));
        let height = short_edge;
        fit_within_source(width, height, source_width, source_height)
    } else {
        fit_within_source(short_edge, short_edge, source_width, source_height)
    }
}

pub(crate) fn target_video_bitrate_kbps(
    base_video_kbps: i32,
    preset_width: i32,
    preset_height: i32,
    width: i32,
    height: i32,
) -> i32 {
    let base_pixels = i64::from(preset_width) * i64::from(preset_height);
    if base_pixels <= 0 {
        return base_video_kbps;
    }
    let target_pixels = i64::from(width) * i64::from(height);
    let scaled = (f64::from(base_video_kbps) * target_pixels as f64 / base_pixels as f64).round();
    even_floor(scaled.max(64.0))
}

pub(crate) fn estimate_transcoded_bytes(
    seconds: f64,
    video_kbps: i32,
    audio_kbps: i32,
    overhead: f64,
) -> i64 {
    if seconds <= 0.0 {
        return 0;
    }
    let bitrate_bps = f64::from(video_kbps + audio_kbps) * 1000.0;
    let media_bytes = seconds * bitrate_bps / 8.0;
    (media_bytes * overhead).ceil().max(1.0) as i64
}

pub(crate) fn enforce_upload_media_limits(
    state: &AppState,
    duration_seconds: f64,
    width: i32,
    height: i32,
) -> Result<(), ApiError> {
    if duration_seconds > state.config.upload_max_duration_seconds {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Video duration exceeds upload limit",
        ));
    }
    let pixel_count = i64::from(width) * i64::from(height);
    let long_edge = i64::from(width.max(height));
    if long_edge > state.config.upload_max_source_long_edge
        || pixel_count > state.config.upload_max_source_pixels
    {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Video resolution exceeds upload limit",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use uuid::Uuid;

    use super::{
        collect_segment_files, estimate_transcoded_bytes, parse_probe_duration,
        stream_rotation_degrees, target_dimensions_for_source, target_video_bitrate_kbps,
    };

    #[test]
    fn target_dimensions_follow_source_orientation() {
        assert_eq!(
            target_dimensions_for_source(1920, 1080, Some((1080, 1920))),
            (1080, 1920)
        );
        assert_eq!(
            target_dimensions_for_source(1920, 1080, Some((1920, 1080))),
            (1920, 1080)
        );
        assert_eq!(
            target_dimensions_for_source(1920, 1080, Some((1600, 1200))),
            (1440, 1080)
        );
        assert_eq!(
            target_dimensions_for_source(1920, 1080, Some((1080, 1080))),
            (1080, 1080)
        );
        assert_eq!(
            target_dimensions_for_source(2560, 1440, Some((3440, 1440))),
            (3440, 1440)
        );
    }

    #[test]
    fn estimate_transcoded_bytes_uses_bitrate_and_overhead() {
        assert_eq!(estimate_transcoded_bytes(1.0, 500, 96, 1.08), 80460);
    }

    #[test]
    fn probe_duration_prefers_stream_then_format_positive_values() {
        let stream = json!({ "duration": "6.25" });
        let data = json!({ "format": { "duration": "9.5" } });
        assert_eq!(parse_probe_duration(&data, &stream), Some(6.25));

        let stream = json!({ "duration": "N/A" });
        let data = json!({ "format": { "duration": 9.5 } });
        assert_eq!(parse_probe_duration(&data, &stream), Some(9.5));

        let stream = json!({ "duration": -1.0 });
        let data = json!({ "format": { "duration": 0.0 } });
        assert_eq!(parse_probe_duration(&data, &stream), None);
    }

    #[test]
    fn stream_rotation_reads_tags_and_side_data() {
        assert_eq!(
            stream_rotation_degrees(&json!({ "tags": { "rotate": "-90" } })),
            270
        );
        assert_eq!(
            stream_rotation_degrees(&json!({
                "side_data_list": [{ "rotation": 90.0 }]
            })),
            90
        );
        assert_eq!(stream_rotation_degrees(&json!({})), 0);
    }

    #[test]
    fn target_video_bitrate_scales_by_rendered_pixels() {
        assert_eq!(target_video_bitrate_kbps(5_000, 1920, 1080, 960, 540), 1250);
        assert_eq!(target_video_bitrate_kbps(10, 1920, 1080, 256, 144), 64);
    }

    #[test]
    fn collect_segment_files_orders_numbered_segments_only() {
        let dir = std::env::temp_dir().join(format!("autvid_segments_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("seg_00010.ts"), b"10").unwrap();
        fs::write(dir.join("seg_00002.ts"), b"2").unwrap();
        fs::write(dir.join("not_a_segment.ts"), b"ignored").unwrap();
        fs::write(dir.join("seg_bad.ts"), b"last").unwrap();
        fs::write(dir.join("seg_00001.mp4"), b"ignored extension").unwrap();

        let files = collect_segment_files(&dir).unwrap();
        let names = files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["seg_00002.ts", "seg_00010.ts", "seg_bad.ts"]);
        let _ = fs::remove_dir_all(dir);
    }
}
