pub(crate) const STATUS_PENDING: &str = "pending";
pub(crate) const STATUS_PROCESSING: &str = "processing";
pub(crate) const STATUS_AWAITING_APPROVAL: &str = "awaiting_approval";
pub(crate) const STATUS_UPLOADING: &str = "uploading";
pub(crate) const STATUS_READY: &str = "ready";
pub(crate) const STATUS_ERROR: &str = "error";
pub(crate) const STATUS_EXPIRED: &str = "expired";
pub(crate) const DEFAULT_API_PORT: u16 = 8000;
pub(crate) const DEFAULT_ADMIN_REFRESH_TOKEN_TTL_HOURS: i64 = 24 * 30;
pub(crate) const CATALOG_CONTENT_TYPE: &str = "application/vnd.autonomi.video.catalog+json;v=1";
pub(crate) const VIDEO_MANIFEST_CONTENT_TYPE: &str =
    "application/vnd.autonomi.video.manifest+json;v=1";
pub(crate) const MIN_ANTD_SELF_ENCRYPTION_BYTES: usize = 3;
pub(crate) const JOB_KIND_PROCESS_VIDEO: &str = "process_video";
pub(crate) const JOB_KIND_UPLOAD_VIDEO: &str = "upload_video";
pub(crate) const JOB_KIND_PUBLISH_CATALOG: &str = "publish_catalog";
pub(crate) const JOB_STATUS_QUEUED: &str = "queued";
pub(crate) const JOB_STATUS_RUNNING: &str = "running";
pub(crate) const JOB_STATUS_SUCCEEDED: &str = "succeeded";
pub(crate) const JOB_STATUS_FAILED: &str = "failed";
pub(crate) const SUPPORTED_RESOLUTIONS: &[&str] = &[
    "8k", "4k", "1440p", "1080p", "720p", "540p", "480p", "360p", "240p", "144p",
];
