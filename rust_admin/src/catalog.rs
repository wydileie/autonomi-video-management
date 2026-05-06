mod db_document;
mod state_file;
mod sync;

pub(crate) use db_document::{
    apply_catalog_visibility, catalog_entry_to_video_out, db_video_to_out, get_db_video,
    manifest_to_video_out,
};
pub(crate) use state_file::{read_catalog_address, read_catalog_state_value};
pub(crate) use sync::{
    ensure_video_manifest_address, load_catalog, load_json_from_autonomi,
    load_video_manifest_by_id, publish_current_catalog_to_network, refresh_local_catalog_from_db,
};

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::SocketAddr,
        path::PathBuf,
        sync::{atomic::AtomicU64, Arc},
    };

    use axum::http::HeaderValue;
    use serde_json::{json, Value};
    use sqlx::postgres::PgPoolOptions;
    use tokio::sync::{Mutex, Semaphore};
    use uuid::Uuid;

    use super::state_file::write_catalog_state;
    use super::*;
    use crate::{
        antd_client::AntdRestClient,
        config::Config,
        metrics::AdminMetrics,
        models::{PublicCatalogDocument, PublicCatalogVariant, PublicCatalogVideo},
        state::AppState,
        CATALOG_CONTENT_TYPE, STATUS_READY,
    };

    fn test_config(catalog_state_path: PathBuf) -> Config {
        Config {
            db_dsn: "postgresql://example".to_string(),
            antd_url: "http://127.0.0.1:0".to_string(),
            antd_payment_mode: "auto".to_string(),
            antd_metadata_payment_mode: "merkle".to_string(),
            admin_username: "admin".to_string(),
            admin_password: "password".to_string(),
            admin_auth_secret: "secret".to_string(),
            admin_auth_ttl_hours: 12,
            admin_auth_cookie_secure: false,
            catalog_state_path,
            catalog_bootstrap_address: None,
            cors_allowed_origins: vec![HeaderValue::from_static("http://localhost")],
            bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            admin_request_timeout_seconds: 120.0,
            admin_upload_request_timeout_seconds: 3600.0,
            upload_temp_dir: std::env::temp_dir(),
            upload_max_file_bytes: 20 * 1024 * 1024,
            upload_min_free_bytes: 0,
            upload_max_concurrent_saves: 1,
            upload_ffprobe_timeout_seconds: 30.0,
            hls_segment_duration: 1.0,
            ffmpeg_threads: 1,
            ffmpeg_filter_threads: 1,
            ffmpeg_max_parallel_renditions: 1,
            upload_max_duration_seconds: 3600.0,
            upload_max_source_pixels: 1920 * 1080,
            upload_max_source_long_edge: 1920,
            upload_quote_transcoded_overhead: 1.08,
            upload_quote_max_sample_bytes: 1024,
            final_quote_approval_ttl_seconds: 3600,
            approval_cleanup_interval_seconds: 300,
            antd_upload_verify: false,
            antd_upload_retries: 1,
            antd_upload_timeout_seconds: 30.0,
            antd_quote_concurrency: 1,
            antd_upload_concurrency: 1,
            antd_approve_on_startup: false,
            antd_require_cost_ready: false,
            antd_direct_upload_max_bytes: 1024,
            admin_job_workers: 1,
            admin_job_poll_interval_seconds: 1,
            admin_job_lease_seconds: 60,
            admin_job_max_attempts: 1,
            catalog_publish_job_max_attempts: 1,
        }
    }

    fn test_state(config: Config) -> AppState {
        let metrics = Arc::new(AdminMetrics::default());
        AppState {
            config: Arc::new(config),
            pool: PgPoolOptions::new()
                .connect_lazy("postgresql://postgres:postgres@localhost/postgres")
                .unwrap(),
            antd: AntdRestClient::new("http://127.0.0.1:9", 1.0, metrics.clone()).unwrap(),
            metrics,
            catalog_lock: Arc::new(Mutex::new(())),
            catalog_publish_lock: Arc::new(Mutex::new(())),
            catalog_publish_epoch: Arc::new(AtomicU64::new(0)),
            upload_save_semaphore: Arc::new(Semaphore::new(1)),
        }
    }

    #[test]
    fn invalid_catalog_state_is_quarantined_to_broken_file() {
        let dir = std::env::temp_dir().join(format!("autvid_catalog_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("catalog.json");
        fs::write(&path, "not valid json").unwrap();
        let config = test_config(path.clone());

        assert!(read_catalog_state_value(&config).is_none());

        assert!(!path.exists());
        assert!(path.with_file_name("catalog.json.broken").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn write_catalog_state_persists_snapshot_and_pending_flag() {
        let dir = std::env::temp_dir().join(format!("autvid_catalog_{}", Uuid::new_v4()));
        let path = dir.join("catalog.json");
        let config = test_config(path.clone());
        let catalog = PublicCatalogDocument {
            schema_version: 1,
            content_type: CATALOG_CONTENT_TYPE.to_string(),
            updated_at: "2026-05-05T00:00:00Z".to_string(),
            videos: vec![PublicCatalogVideo {
                id: "video-1".to_string(),
                title: "Example".to_string(),
                original_filename: None,
                description: None,
                status: STATUS_READY.to_string(),
                created_at: "2026-05-05T00:00:00Z".to_string(),
                updated_at: "2026-05-05T00:00:01Z".to_string(),
                manifest_address: "manifest-address".to_string(),
                show_original_filename: false,
                show_manifest_address: false,
                variants: vec![PublicCatalogVariant {
                    resolution: "720p".to_string(),
                    width: 1280,
                    height: 720,
                    segment_count: 1,
                    total_duration: Some(6.0),
                }],
            }],
        };

        write_catalog_state(&config, Some("catalog-address"), Some(&catalog), true).unwrap();

        let payload: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(payload["catalog_address"], "catalog-address");
        assert_eq!(payload["publish_pending"], true);
        assert_eq!(payload["catalog"]["videos"][0]["id"], "video-1");
        assert!(!path.with_extension("tmp").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn catalog_entry_to_video_out_applies_public_visibility() {
        let entry = json!({
            "id": "video-1",
            "title": "Public Video",
            "description": "Visible summary",
            "status": STATUS_READY,
            "created_at": "2026-05-05T00:00:00Z",
            "manifest_address": "manifest-address",
            "show_manifest_address": false,
            "variants": [{
                "resolution": "720p",
                "width": 1280,
                "height": 720,
                "segment_count": 2,
                "total_duration": 12.5
            }]
        });

        let video = catalog_entry_to_video_out(&entry, Some("catalog-address"));

        assert_eq!(video.id, "video-1");
        assert_eq!(video.manifest_address, None);
        assert_eq!(video.catalog_address.as_deref(), Some("catalog-address"));
        assert!(video.is_public);
        assert!(!video.show_original_filename);
        assert_eq!(video.variants.len(), 1);
        assert_eq!(video.variants[0].segment_count, Some(2));
    }

    #[tokio::test]
    async fn manifest_to_video_out_hides_sensitive_fields_for_public_view() {
        let dir = std::env::temp_dir().join(format!("autvid_catalog_{}", Uuid::new_v4()));
        let mut config = test_config(dir.join("catalog.json"));
        config.catalog_bootstrap_address = Some("catalog-bootstrap".to_string());
        let state = test_state(config);
        let manifest = json!({
            "id": "video-1",
            "title": "Demo",
            "original_filename": "raw-source.mov",
            "description": "Internal upload",
            "status": STATUS_READY,
            "created_at": "2026-05-05T00:00:00Z",
            "show_original_filename": true,
            "show_manifest_address": false,
            "original_file": {
                "autonomi_address": "original-address",
                "byte_size": 12345
            },
            "variants": [{
                "resolution": "720p",
                "width": 1280,
                "height": 720,
                "segment_count": 1,
                "total_duration": 6.0,
                "segments": [{
                    "segment_index": 0,
                    "autonomi_address": "segment-address",
                    "duration": 6.0
                }]
            }]
        });

        let admin_view = manifest_to_video_out(&state, &manifest, Some("manifest-address"), false);
        assert_eq!(
            admin_view.original_filename.as_deref(),
            Some("raw-source.mov")
        );
        assert_eq!(
            admin_view.manifest_address.as_deref(),
            Some("manifest-address")
        );
        assert_eq!(
            admin_view.catalog_address.as_deref(),
            Some("catalog-bootstrap")
        );
        assert_eq!(
            admin_view.original_file_address.as_deref(),
            Some("original-address")
        );
        assert_eq!(admin_view.variants[0].segments.len(), 1);

        let public_view = manifest_to_video_out(&state, &manifest, Some("manifest-address"), true);
        assert_eq!(public_view.original_filename, None);
        assert_eq!(public_view.manifest_address, None);
        assert_eq!(public_view.catalog_address, None);
        assert_eq!(public_view.original_file_address, None);
        assert!(public_view.variants[0].segments.is_empty());
    }
}
