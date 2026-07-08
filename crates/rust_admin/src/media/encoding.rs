use axum::http::StatusCode;

use crate::{
    errors::ApiError,
    models::{EncodeSettings, VideoCodec},
    state::AppState,
    SUPPORTED_RESOLUTIONS,
};

pub(crate) const MIN_VIDEO_BITRATE_KBPS: i32 = 64;
pub(crate) const MAX_VIDEO_BITRATE_KBPS: i32 = 200_000;
pub(crate) const MIN_AUDIO_BITRATE_KBPS: i32 = 32;
pub(crate) const MAX_AUDIO_BITRATE_KBPS: i32 = 1_024;

pub(crate) fn resolution_preset(resolution: &str) -> Option<(i32, i32, i32, i32)> {
    match resolution {
        "8k" => Some((7680, 4320, 80_000, 320)),
        "4k" => Some((3840, 2160, 45_000, 320)),
        "1440p" => Some((2560, 1440, 24_000, 320)),
        "1080p" => Some((1920, 1080, 12_000, 256)),
        "720p" => Some((1280, 720, 7_500, 192)),
        "540p" => Some((960, 540, 4_500, 160)),
        "480p" => Some((854, 480, 3_000, 160)),
        "360p" => Some((640, 360, 1_500, 128)),
        "240p" => Some((426, 240, 800, 96)),
        "144p" => Some((256, 144, 350, 64)),
        _ => None,
    }
}

pub(crate) fn validate_encode_settings(
    settings: &EncodeSettings,
    selected_resolutions: &[String],
) -> Result<(), ApiError> {
    if let Some(audio_kbps) = settings.audio_bitrate_kbps {
        if !(MIN_AUDIO_BITRATE_KBPS..=MAX_AUDIO_BITRATE_KBPS).contains(&audio_kbps) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "audio_bitrate_kbps must be between {MIN_AUDIO_BITRATE_KBPS} and {MAX_AUDIO_BITRATE_KBPS}"
                ),
            ));
        }
    }

    for (resolution, video_kbps) in &settings.video_bitrate_overrides {
        if !selected_resolutions.iter().any(|value| value == resolution) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("video bitrate override references unselected resolution '{resolution}'"),
            ));
        }
        if resolution_preset(resolution).is_none() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "video bitrate override references unsupported resolution '{resolution}'. {}",
                    supported_resolutions_error()
                ),
            ));
        }
        let max_video_bitrate_kbps = max_video_bitrate_override_kbps(resolution);
        if !(MIN_VIDEO_BITRATE_KBPS..=max_video_bitrate_kbps).contains(video_kbps) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "video bitrate for {resolution} must be between {MIN_VIDEO_BITRATE_KBPS} and {max_video_bitrate_kbps} kbps"
                ),
            ));
        }
    }

    Ok(())
}

pub(crate) fn default_video_bitrate_for_codec(base_video_kbps: i32, codec: VideoCodec) -> i32 {
    match codec {
        VideoCodec::H264 => base_video_kbps,
        VideoCodec::Hevc => (base_video_kbps * 7 / 10).max(MIN_VIDEO_BITRATE_KBPS),
    }
}

pub(crate) fn max_video_bitrate_override_kbps(resolution: &str) -> i32 {
    resolution_preset(resolution)
        .map(|(_, _, video_kbps, _)| (video_kbps * 4).min(MAX_VIDEO_BITRATE_KBPS))
        .unwrap_or(MAX_VIDEO_BITRATE_KBPS)
}

pub(crate) fn supported_resolutions_error() -> String {
    let values = SUPPORTED_RESOLUTIONS
        .iter()
        .map(|resolution| format!("'{resolution}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("No valid resolutions. Choose from: [{values}]")
}

pub(crate) fn even_floor(value: f64) -> i32 {
    let floored = value.floor().max(2.0) as i32;
    let even = floored - floored.rem_euclid(2);
    even.max(2)
}

pub(crate) fn fit_within_source(
    width: i32,
    height: i32,
    source_width: i32,
    source_height: i32,
) -> (i32, i32) {
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
