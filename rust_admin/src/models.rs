use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{JOB_KIND_PROCESS_VIDEO, JOB_KIND_PUBLISH_CATALOG, JOB_KIND_UPLOAD_VIDEO};

#[derive(Serialize)]
pub(crate) struct HealthResponse {
    pub(crate) ok: bool,
    pub(crate) autonomi: AutonomiHealth,
    pub(crate) postgres: PostgresHealth,
    pub(crate) write_ready: bool,
    pub(crate) payment_mode: String,
    pub(crate) final_quote_approval_ttl_seconds: i64,
    pub(crate) implementation: &'static str,
    pub(crate) role: &'static str,
}

#[derive(Serialize)]
pub(crate) struct AutonomiHealth {
    pub(crate) ok: bool,
    pub(crate) network: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct PostgresHealth {
    pub(crate) ok: bool,
    pub(crate) error: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct VideoVisibilityUpdate {
    #[serde(default, rename = "show_original_filename")]
    pub(crate) _show_original_filename: bool,
    pub(crate) show_manifest_address: bool,
}

#[derive(Deserialize)]
pub(crate) struct VideoPublicationUpdate {
    pub(crate) is_public: bool,
}

#[derive(Deserialize)]
pub(crate) struct UploadQuoteRequest {
    pub(crate) duration_seconds: f64,
    pub(crate) resolutions: Vec<String>,
    pub(crate) source_width: Option<i32>,
    pub(crate) source_height: Option<i32>,
    #[serde(default)]
    pub(crate) upload_original: bool,
    pub(crate) source_size_bytes: Option<i64>,
}

#[derive(Serialize, Clone)]
pub(crate) struct SegmentOut {
    pub(crate) segment_index: i32,
    pub(crate) autonomi_address: Option<String>,
    pub(crate) duration: f64,
}

#[derive(Serialize, Clone)]
pub(crate) struct VariantOut {
    pub(crate) id: String,
    pub(crate) resolution: String,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) total_duration: Option<f64>,
    pub(crate) segment_count: Option<i32>,
    pub(crate) segments: Vec<SegmentOut>,
}

#[derive(Serialize, Clone)]
pub(crate) struct VideoOut {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) original_filename: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) status: String,
    pub(crate) created_at: String,
    pub(crate) manifest_address: Option<String>,
    pub(crate) catalog_address: Option<String>,
    pub(crate) is_public: bool,
    pub(crate) show_original_filename: bool,
    pub(crate) show_manifest_address: bool,
    pub(crate) upload_original: bool,
    pub(crate) original_file_address: Option<String>,
    pub(crate) original_file_byte_size: Option<i64>,
    pub(crate) publish_when_ready: bool,
    pub(crate) error_message: Option<String>,
    pub(crate) final_quote: Option<Value>,
    pub(crate) final_quote_created_at: Option<String>,
    pub(crate) approval_expires_at: Option<String>,
    pub(crate) variants: Vec<VariantOut>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct PublicCatalogDocument {
    pub(crate) schema_version: i32,
    pub(crate) content_type: String,
    pub(crate) updated_at: String,
    #[serde(default)]
    pub(crate) videos: Vec<PublicCatalogVideo>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct PublicCatalogVideo {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) original_filename: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) status: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) manifest_address: String,
    pub(crate) show_original_filename: bool,
    pub(crate) show_manifest_address: bool,
    #[serde(default)]
    pub(crate) variants: Vec<PublicCatalogVariant>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct PublicCatalogVariant {
    pub(crate) resolution: String,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) segment_count: i32,
    pub(crate) total_duration: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct VideoManifestDocument {
    pub(crate) schema_version: i32,
    pub(crate) content_type: String,
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) original_filename: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) status: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) show_original_filename: bool,
    pub(crate) show_manifest_address: bool,
    pub(crate) original_file: Option<ManifestOriginalFile>,
    #[serde(default)]
    pub(crate) variants: Vec<ManifestVariant>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ManifestOriginalFile {
    pub(crate) autonomi_address: String,
    pub(crate) byte_size: Option<i64>,
    pub(crate) autonomi_cost_atto: Option<String>,
    pub(crate) payment_mode: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ManifestVariant {
    pub(crate) id: String,
    pub(crate) resolution: String,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) video_bitrate: i32,
    pub(crate) audio_bitrate: i32,
    pub(crate) segment_duration: f64,
    pub(crate) total_duration: Option<f64>,
    pub(crate) segment_count: usize,
    #[serde(default)]
    pub(crate) segments: Vec<ManifestSegment>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ManifestSegment {
    pub(crate) segment_index: i32,
    pub(crate) autonomi_address: Option<String>,
    pub(crate) duration: f64,
    pub(crate) byte_size: Option<i64>,
}

#[derive(Serialize)]
pub(crate) struct UploadQuoteVariantOut {
    pub(crate) resolution: String,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) segment_count: i64,
    pub(crate) estimated_bytes: i64,
    pub(crate) chunk_count: i64,
    pub(crate) storage_cost_atto: String,
    pub(crate) estimated_gas_cost_wei: String,
    pub(crate) payment_mode: String,
}

#[derive(Serialize)]
pub(crate) struct UploadQuoteOriginalOut {
    pub(crate) estimated_bytes: i64,
    pub(crate) chunk_count: i64,
    pub(crate) storage_cost_atto: String,
    pub(crate) estimated_gas_cost_wei: String,
    pub(crate) payment_mode: String,
}

#[derive(Serialize)]
pub(crate) struct UploadQuoteOut {
    pub(crate) duration_seconds: f64,
    pub(crate) segment_duration: f64,
    pub(crate) payment_mode: String,
    pub(crate) estimated_bytes: i64,
    pub(crate) segment_count: i64,
    pub(crate) storage_cost_atto: String,
    pub(crate) estimated_gas_cost_wei: String,
    pub(crate) metadata_bytes: i64,
    pub(crate) sampled: bool,
    pub(crate) original_file: Option<UploadQuoteOriginalOut>,
    pub(crate) variants: Vec<UploadQuoteVariantOut>,
}

pub(crate) struct AcceptedUpload {
    pub(crate) video_id: String,
    pub(crate) video: VideoOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JobKind {
    ProcessVideo,
    UploadVideo,
    PublishCatalog,
}

impl JobKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ProcessVideo => JOB_KIND_PROCESS_VIDEO,
            Self::UploadVideo => JOB_KIND_UPLOAD_VIDEO,
            Self::PublishCatalog => JOB_KIND_PUBLISH_CATALOG,
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            JOB_KIND_PROCESS_VIDEO => Some(Self::ProcessVideo),
            JOB_KIND_UPLOAD_VIDEO => Some(Self::UploadVideo),
            JOB_KIND_PUBLISH_CATALOG => Some(Self::PublishCatalog),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LeasedJob {
    pub(crate) id: Uuid,
    pub(crate) kind: JobKind,
    pub(crate) video_id: Option<Uuid>,
    pub(crate) attempts: i32,
    pub(crate) max_attempts: i32,
}

pub(crate) struct UploadMediaMetadata {
    pub(crate) duration_seconds: f64,
    pub(crate) dimensions: (i32, i32),
}

pub(crate) struct CommandOutput {
    pub(crate) status_code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

pub(crate) struct TranscodedRendition {
    pub(crate) order: usize,
    pub(crate) resolution: String,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) video_kbps: i32,
    pub(crate) audio_kbps: i32,
    pub(crate) segments: Vec<TranscodedSegment>,
}

pub(crate) struct TranscodedSegment {
    pub(crate) segment_index: i32,
    pub(crate) duration: f64,
    pub(crate) byte_size: i64,
    pub(crate) local_path: PathBuf,
}

#[derive(Clone)]
pub(crate) struct QuoteValue {
    pub(crate) sampled: bool,
    pub(crate) storage_cost_atto: u128,
    pub(crate) estimated_gas_cost_wei: u128,
    pub(crate) chunk_count: i64,
    pub(crate) payment_mode: String,
}

#[cfg(test)]
mod tests {
    use super::{
        JobKind, ManifestSegment, ManifestVariant, PublicCatalogDocument, PublicCatalogVariant,
        PublicCatalogVideo, VideoManifestDocument,
    };
    use crate::{JOB_KIND_PROCESS_VIDEO, JOB_KIND_PUBLISH_CATALOG, JOB_KIND_UPLOAD_VIDEO};
    use serde_json::json;

    #[test]
    fn parses_durable_job_kinds() {
        assert_eq!(
            JobKind::parse(JOB_KIND_PROCESS_VIDEO),
            Some(JobKind::ProcessVideo)
        );
        assert_eq!(
            JobKind::parse(JOB_KIND_UPLOAD_VIDEO),
            Some(JobKind::UploadVideo)
        );
        assert_eq!(
            JobKind::parse(JOB_KIND_PUBLISH_CATALOG),
            Some(JobKind::PublishCatalog)
        );
        assert_eq!(JobKind::parse("unknown"), None);
    }

    #[test]
    fn public_catalog_document_preserves_json_contract_shape() {
        let catalog = PublicCatalogDocument {
            schema_version: 1,
            content_type: "application/vnd.autonomi.video.catalog+json;v=1".to_string(),
            updated_at: "2026-05-05T00:00:00Z".to_string(),
            videos: vec![PublicCatalogVideo {
                id: "video-1".to_string(),
                title: "Example".to_string(),
                original_filename: None,
                description: Some("Demo".to_string()),
                status: "ready".to_string(),
                created_at: "2026-05-05T00:00:00Z".to_string(),
                updated_at: "2026-05-05T00:00:00Z".to_string(),
                manifest_address: "abc123".to_string(),
                show_original_filename: false,
                show_manifest_address: true,
                variants: vec![PublicCatalogVariant {
                    resolution: "720p".to_string(),
                    width: 1280,
                    height: 720,
                    segment_count: 2,
                    total_duration: Some(12.5),
                }],
            }],
        };

        let value = serde_json::to_value(catalog).unwrap();
        assert_eq!(
            value,
            json!({
                "schema_version": 1,
                "content_type": "application/vnd.autonomi.video.catalog+json;v=1",
                "updated_at": "2026-05-05T00:00:00Z",
                "videos": [{
                    "id": "video-1",
                    "title": "Example",
                    "original_filename": null,
                    "description": "Demo",
                    "status": "ready",
                    "created_at": "2026-05-05T00:00:00Z",
                    "updated_at": "2026-05-05T00:00:00Z",
                    "manifest_address": "abc123",
                    "show_original_filename": false,
                    "show_manifest_address": true,
                    "variants": [{
                        "resolution": "720p",
                        "width": 1280,
                        "height": 720,
                        "segment_count": 2,
                        "total_duration": 12.5
                    }]
                }]
            })
        );
    }

    #[test]
    fn video_manifest_document_preserves_json_contract_shape() {
        let manifest = VideoManifestDocument {
            schema_version: 1,
            content_type: "application/vnd.autonomi.video.manifest+json;v=1".to_string(),
            id: "video-1".to_string(),
            title: "Example".to_string(),
            original_filename: None,
            description: None,
            status: "ready".to_string(),
            created_at: "2026-05-05T00:00:00Z".to_string(),
            updated_at: "2026-05-05T00:00:01Z".to_string(),
            show_original_filename: false,
            show_manifest_address: false,
            original_file: None,
            variants: vec![ManifestVariant {
                id: "variant-1".to_string(),
                resolution: "720p".to_string(),
                width: 1280,
                height: 720,
                video_bitrate: 2_500_000,
                audio_bitrate: 128_000,
                segment_duration: 6.0,
                total_duration: Some(12.0),
                segment_count: 1,
                segments: vec![ManifestSegment {
                    segment_index: 0,
                    autonomi_address: Some("segment-address".to_string()),
                    duration: 6.0,
                    byte_size: Some(1024),
                }],
            }],
        };

        let value = serde_json::to_value(manifest).unwrap();
        assert_eq!(
            value,
            json!({
                "schema_version": 1,
                "content_type": "application/vnd.autonomi.video.manifest+json;v=1",
                "id": "video-1",
                "title": "Example",
                "original_filename": null,
                "description": null,
                "status": "ready",
                "created_at": "2026-05-05T00:00:00Z",
                "updated_at": "2026-05-05T00:00:01Z",
                "show_original_filename": false,
                "show_manifest_address": false,
                "original_file": null,
                "variants": [{
                    "id": "variant-1",
                    "resolution": "720p",
                    "width": 1280,
                    "height": 720,
                    "video_bitrate": 2500000,
                    "audio_bitrate": 128000,
                    "segment_duration": 6.0,
                    "total_duration": 12.0,
                    "segment_count": 1,
                    "segments": [{
                        "segment_index": 0,
                        "autonomi_address": "segment-address",
                        "duration": 6.0,
                        "byte_size": 1024
                    }]
                }]
            })
        );
    }
}
