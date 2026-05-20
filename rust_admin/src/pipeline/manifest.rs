use chrono::Utc;

use crate::{
    models::{ManifestOriginalFile, ManifestSegment, ManifestVariant, VideoManifestDocument},
    STATUS_READY, VIDEO_MANIFEST_CONTENT_TYPE,
};

pub(super) struct OriginalFileManifestInput {
    pub(super) byte_size: Option<i64>,
    pub(super) autonomi_cost_atto: Option<String>,
    pub(super) payment_mode: Option<String>,
}

pub(super) struct SegmentManifestInput {
    pub(super) segment_index: i32,
    pub(super) autonomi_address: Option<String>,
    pub(super) duration: f64,
    pub(super) byte_size: Option<i64>,
}

pub(super) struct VariantManifestInput {
    pub(super) id: String,
    pub(super) resolution: String,
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) video_bitrate: i32,
    pub(super) audio_bitrate: i32,
    pub(super) segment_duration: f64,
    pub(super) total_duration: Option<f64>,
}

pub(super) struct VideoManifestInput {
    pub(super) title: String,
    pub(super) description: Option<String>,
}

pub(super) fn manifest_original_file(
    address: String,
    input: OriginalFileManifestInput,
) -> ManifestOriginalFile {
    ManifestOriginalFile {
        autonomi_address: address,
        byte_size: input.byte_size,
        autonomi_cost_atto: input.autonomi_cost_atto,
        payment_mode: input.payment_mode,
    }
}

pub(super) fn manifest_variant(
    variant: VariantManifestInput,
    uploaded_segments: Vec<SegmentManifestInput>,
) -> ManifestVariant {
    let segment_count = uploaded_segments.len();
    let segments = uploaded_segments
        .into_iter()
        .map(|segment| ManifestSegment {
            segment_index: segment.segment_index,
            autonomi_address: segment.autonomi_address,
            duration: segment.duration,
            byte_size: segment.byte_size,
        })
        .collect();

    ManifestVariant {
        id: variant.id,
        resolution: variant.resolution,
        width: variant.width,
        height: variant.height,
        video_bitrate: variant.video_bitrate,
        audio_bitrate: variant.audio_bitrate,
        segment_duration: variant.segment_duration,
        total_duration: variant.total_duration,
        segment_count,
        segments,
    }
}

pub(super) fn video_manifest_document(
    video_id: &str,
    video: VideoManifestInput,
    manifest_created_at: String,
    show_manifest_address: bool,
    original_file: Option<ManifestOriginalFile>,
    variants: Vec<ManifestVariant>,
) -> VideoManifestDocument {
    VideoManifestDocument {
        schema_version: 1,
        content_type: VIDEO_MANIFEST_CONTENT_TYPE.to_string(),
        id: video_id.to_string(),
        title: video.title,
        original_filename: None,
        description: video.description,
        status: STATUS_READY.to_string(),
        created_at: manifest_created_at,
        updated_at: Utc::now().to_rfc3339(),
        show_original_filename: false,
        show_manifest_address,
        original_file,
        variants,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_variant_preserves_segment_order_and_summary_fields() {
        let variant = manifest_variant(
            VariantManifestInput {
                id: "variant-1".to_string(),
                resolution: "720p".to_string(),
                width: 1280,
                height: 720,
                video_bitrate: 2_500_000,
                audio_bitrate: 128_000,
                segment_duration: 1.0,
                total_duration: Some(2.0),
            },
            vec![
                SegmentManifestInput {
                    segment_index: 0,
                    autonomi_address: Some("addr-0".to_string()),
                    duration: 1.0,
                    byte_size: Some(1024),
                },
                SegmentManifestInput {
                    segment_index: 1,
                    autonomi_address: Some("addr-1".to_string()),
                    duration: 1.0,
                    byte_size: Some(2048),
                },
            ],
        );

        assert_eq!(variant.id, "variant-1");
        assert_eq!(variant.resolution, "720p");
        assert_eq!(variant.segment_count, 2);
        assert_eq!(
            variant.segments[0].autonomi_address.as_deref(),
            Some("addr-0")
        );
        assert_eq!(variant.segments[1].byte_size, Some(2048));
    }

    #[test]
    fn video_manifest_document_uses_ready_contract() {
        let manifest = video_manifest_document(
            "video-1",
            VideoManifestInput {
                title: "Video".to_string(),
                description: Some("Description".to_string()),
            },
            "2026-05-18T00:00:00Z".to_string(),
            true,
            Some(manifest_original_file(
                "original-address".to_string(),
                OriginalFileManifestInput {
                    byte_size: Some(4096),
                    autonomi_cost_atto: Some("123".to_string()),
                    payment_mode: Some("auto".to_string()),
                },
            )),
            Vec::new(),
        );

        assert_eq!(manifest.id, "video-1");
        assert_eq!(manifest.status, STATUS_READY);
        assert!(manifest.show_manifest_address);
        assert_eq!(
            manifest.original_file.unwrap().autonomi_address,
            "original-address"
        );
    }
}
