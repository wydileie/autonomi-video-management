#![allow(clippy::unwrap_used)]
use std::fs;
use std::path::Path;

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use super::{
    assert_under, collect_segment_files, estimate_transcoded_bytes, ffmpeg_transcode_args,
    parse_probe_duration, stream_rotation_degrees, target_dimensions_for_source,
    target_video_bitrate_kbps, validate_encode_settings, FfmpegTranscodeOptions,
};
use crate::models::{EncodeSettings, VideoCodec};

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
fn validate_encode_settings_rejects_unselected_and_excessive_bitrates() {
    let selected = vec!["720p".to_string()];
    let mut settings = EncodeSettings::default();
    settings
        .video_bitrate_overrides
        .insert("1080p".to_string(), 12_000);
    let err = validate_encode_settings(&settings, &selected).unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.detail.contains("unselected resolution"));

    let mut settings = EncodeSettings::default();
    settings
        .video_bitrate_overrides
        .insert("144p".to_string(), 200_000);
    let err = validate_encode_settings(&settings, &["144p".to_string()]).unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.detail.contains("between 64 and 1400"));
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

#[test]
fn ffmpeg_segments_keep_continuous_timestamps_for_hls_seeks() {
    let args = ffmpeg_transcode_args(FfmpegTranscodeOptions {
        src: Path::new("/tmp/source.mp4"),
        segment_pattern: Path::new("/tmp/seg_%05d.ts"),
        filter_threads: 1,
        ffmpeg_threads: 2,
        hls_segment_duration: 1.0,
        video_codec: VideoCodec::H264,
        width: 640,
        height: 360,
        video_kbps: 500,
        audio_kbps: 96,
    })
    .into_iter()
    .map(|arg| arg.to_string_lossy().to_string())
    .collect::<Vec<_>>();
    let reset_timestamp_index = args
        .iter()
        .position(|arg| arg == "-reset_timestamps")
        .expect("reset timestamp flag should be explicit");

    assert_eq!(
        args.get(reset_timestamp_index + 1).map(String::as_str),
        Some("0")
    );
}

#[test]
fn ffmpeg_hevc_args_use_x265_and_hvc1_tag() {
    let args = ffmpeg_transcode_args(FfmpegTranscodeOptions {
        src: Path::new("/tmp/source.mp4"),
        segment_pattern: Path::new("/tmp/seg_%05d.ts"),
        filter_threads: 1,
        ffmpeg_threads: 2,
        hls_segment_duration: 1.0,
        video_codec: VideoCodec::Hevc,
        width: 640,
        height: 360,
        video_kbps: 500,
        audio_kbps: 96,
    })
    .into_iter()
    .map(|arg| arg.to_string_lossy().to_string())
    .collect::<Vec<_>>();

    assert!(args.windows(2).any(|pair| pair == ["-c:v", "libx265"]));
    assert!(args.windows(2).any(|pair| pair == ["-tag:v", "hvc1"]));
}

#[test]
fn assert_under_treats_workspace_escape_as_server_integrity_error() {
    let base = std::env::temp_dir().join(format!("autvid_assert_under_{}", Uuid::new_v4()));
    let root = base.join("upload-temp");
    let outside = base.join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();

    let err = assert_under(&outside.join("source.mp4"), &root).unwrap_err();

    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    let _ = fs::remove_dir_all(base);
}
