use std::{ffi::OsString, fs, path::Path as FsPath, sync::Arc, time::Instant};

use axum::http::StatusCode;
use tokio::{process::Command, sync::Semaphore, task::JoinSet};
use tracing::{info, instrument};

use crate::{
    errors::ApiError,
    models::{EncodeSettings, TranscodedRendition, TranscodedSegment, VideoCodec},
    state::AppState,
    MIN_ANTD_SELF_ENCRYPTION_BYTES,
};

use super::*;

pub(crate) async fn run_ffmpeg(
    state: &AppState,
    src: &FsPath,
    seg_dir: &FsPath,
    target: &RenditionTarget,
) -> Result<(), ApiError> {
    let src = assert_under(src, &state.config.upload_temp_dir)?;
    let seg_dir = assert_under(seg_dir, &state.config.upload_temp_dir)?;
    let segment_pattern =
        assert_under(&seg_dir.join("seg_%05d.ts"), &state.config.upload_temp_dir)?;
    let mut command = Command::new(&state.config.ffmpeg_bin);
    command.args(ffmpeg_transcode_args(FfmpegTranscodeOptions {
        src: &src,
        segment_pattern: &segment_pattern,
        filter_threads: state.config.ffmpeg_filter_threads,
        ffmpeg_threads: state.config.ffmpeg_threads,
        hls_segment_duration: state.config.hls_segment_duration,
        video_codec: target.video_codec,
        width: target.width,
        height: target.height,
        video_kbps: target.video_kbps,
        audio_kbps: target.audio_kbps,
    }));
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

pub(crate) struct FfmpegTranscodeOptions<'a> {
    pub(crate) src: &'a FsPath,
    pub(crate) segment_pattern: &'a FsPath,
    pub(crate) filter_threads: usize,
    pub(crate) ffmpeg_threads: usize,
    pub(crate) hls_segment_duration: f64,
    pub(crate) video_codec: VideoCodec,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) video_kbps: i32,
    pub(crate) audio_kbps: i32,
}

#[derive(Clone)]
pub(crate) struct TranscodeRenditionInput {
    order: usize,
    resolution: String,
    source_dimensions: Option<(i32, i32)>,
    encode_settings: EncodeSettings,
}

pub(crate) struct RenditionTarget {
    video_codec: VideoCodec,
    width: i32,
    height: i32,
    video_kbps: i32,
    audio_kbps: i32,
}

pub(crate) fn ffmpeg_transcode_args(options: FfmpegTranscodeOptions<'_>) -> Vec<OsString> {
    let segment_time = format!("{}", F64Format(options.hls_segment_duration));
    let codec_args: Vec<OsString> = match options.video_codec {
        VideoCodec::H264 => [
            "-c:v",
            "libx264",
            "-threads",
            &options.ffmpeg_threads.to_string(),
            "-preset",
            "veryfast",
            "-profile:v",
            "high",
            "-pix_fmt",
            "yuv420p",
        ]
        .into_iter()
        .map(OsString::from)
        .collect(),
        VideoCodec::Hevc => [
            "-c:v",
            "libx265",
            "-threads",
            &options.ffmpeg_threads.to_string(),
            "-preset",
            "medium",
            "-tag:v",
            "hvc1",
            "-pix_fmt",
            "yuv420p",
            "-x265-params",
            "log-level=error",
        ]
        .into_iter()
        .map(OsString::from)
        .collect(),
    };

    [
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "warning",
        "-y",
        "-filter_threads",
        &options.filter_threads.to_string(),
        "-i",
    ]
    .into_iter()
    .map(OsString::from)
    .chain([options.src.as_os_str().to_owned()])
    .chain(
        ["-map", "0:v:0", "-map", "0:a?", "-sn"]
            .into_iter()
            .map(OsString::from),
    )
    .chain(codec_args)
    .chain(
        [
            "-vf",
            &format!(
                "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2",
                options.width, options.height, options.width, options.height
            ),
            "-b:v",
            &format!("{}k", options.video_kbps),
            "-maxrate",
            &format!("{}k", options.video_kbps * 3 / 2),
            "-bufsize",
            &format!("{}k", options.video_kbps * 2),
            "-force_key_frames",
            &format!("expr:gte(t,n_forced*{segment_time})"),
            "-sc_threshold",
            "0",
            "-c:a",
            "aac",
            "-b:a",
            &format!("{}k", options.audio_kbps),
            "-ar",
            "44100",
            "-f",
            "segment",
            "-segment_time",
            &segment_time,
            "-segment_time_delta",
            "0.05",
            "-segment_format",
            "mpegts",
            "-reset_timestamps",
            "0",
        ]
        .into_iter()
        .map(OsString::from),
    )
    .chain([options.segment_pattern.as_os_str().to_owned()])
    .collect()
}

pub(crate) struct F64Format(f64);

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

pub(crate) fn stderr_tail(stderr: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(stderr);
    let start = text.len().saturating_sub(limit);
    text[start..].trim().to_string()
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
    encode_settings: &EncodeSettings,
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
        let encode_settings = encode_settings.clone();
        scheduled += 1;
        jobs.spawn(async move {
            let _permit = semaphore.acquire_owned().await.map_err(|err| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Could not acquire FFmpeg rendition slot: {err}"),
                )
            })?;
            let input = TranscodeRenditionInput {
                order,
                resolution,
                source_dimensions,
                encode_settings,
            };
            transcode_one_rendition(&state, &video_id, &source_path, &job_dir, input).await
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

#[instrument(skip(state, source_path, job_dir, input), fields(video_id = %video_id))]
pub(crate) async fn transcode_one_rendition(
    state: &AppState,
    video_id: &str,
    source_path: &FsPath,
    job_dir: &FsPath,
    input: TranscodeRenditionInput,
) -> Result<TranscodedRendition, ApiError> {
    let TranscodeRenditionInput {
        order,
        resolution,
        source_dimensions,
        encode_settings,
    } = input;
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
    let base_video_kbps = encode_settings
        .video_bitrate_overrides
        .get(&resolution)
        .copied()
        .unwrap_or_else(|| {
            default_video_bitrate_for_codec(video_kbps, encode_settings.video_codec)
        });
    let video_kbps =
        target_video_bitrate_kbps(base_video_kbps, preset_width, preset_height, width, height);
    let audio_kbps = encode_settings.audio_bitrate_kbps.unwrap_or(audio_kbps);
    let target = RenditionTarget {
        video_codec: encode_settings.video_codec,
        width,
        height,
        video_kbps,
        audio_kbps,
    };
    let seg_dir = job_dir.join(&resolution);
    fs::create_dir_all(&seg_dir).map_err(|err| {
        ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!("Could not create segment directory: {err}"),
        )
    })?;
    info!("Transcoding {} -> {}", video_id, resolution);
    let ffmpeg_started = Instant::now();
    let ffmpeg_result = run_ffmpeg(state, source_path, &seg_dir, &target).await;
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
        width: target.width,
        height: target.height,
        video_kbps: target.video_kbps,
        audio_kbps: target.audio_kbps,
        video_codec: target.video_codec,
        segment_container: "mpegts".to_string(),
        segments,
    })
}
