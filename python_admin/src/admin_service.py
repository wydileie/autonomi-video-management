"""
Python Admin Service — video ingestion, FFmpeg transcoding, Autonomi upload.

Flow:
  1. Client POSTs a video file + desired resolutions.
  2. We save it to /tmp, create a DB record, and kick off background processing.
  3. Background task runs FFmpeg to produce HLS .ts segments per resolution.
  4. The real segment bytes are quoted and the job waits for user approval.
  5. Approved segments are uploaded to the Autonomi network via the antd daemon.
  6. Segment addresses are published in a video manifest on Autonomi.
  7. The network catalog is updated with that manifest address.
  8. The Rust streaming service reads the catalog/manifest from Autonomi.
"""

import asyncio
import json
import logging
import math
import os
import re
import shutil
import secrets
import uuid
from contextlib import asynccontextmanager
from datetime import datetime, timedelta, timezone
from pathlib import Path
from urllib.parse import urlparse

import asyncpg
from fastapi import Depends, FastAPI, File, Form, Header, HTTPException, UploadFile
from fastapi.middleware.cors import CORSMiddleware
from jose import JWTError, jwt
from pydantic import BaseModel

from .antd_client import AntdError, AsyncAntdClient

# ── Config ────────────────────────────────────────────────────────────────────

DB_DSN = (
    f"postgresql://{os.environ['ADMIN_DB_USER']}:{os.environ['ADMIN_DB_PASS']}"
    f"@{os.environ['ADMIN_DB_HOST']}:{os.environ.get('ADMIN_DB_PORT', '5432')}"
    f"/{os.environ['ADMIN_DB_NAME']}"
)
ANTD_URL = os.environ.get("ANTD_URL", "http://localhost:8082")
ANTD_PAYMENT_MODE = os.environ.get("ANTD_PAYMENT_MODE", "auto").strip().lower()
ANTD_UPLOAD_VERIFY = os.environ.get("ANTD_UPLOAD_VERIFY", "true").lower() not in {
    "0",
    "false",
    "no",
}
ANTD_UPLOAD_RETRIES = int(os.environ.get("ANTD_UPLOAD_RETRIES", "3"))
ANTD_UPLOAD_TIMEOUT_SECONDS = float(os.environ.get("ANTD_UPLOAD_TIMEOUT_SECONDS", "120"))
ANTD_APPROVE_ON_STARTUP = os.environ.get("ANTD_APPROVE_ON_STARTUP", "true").lower() not in {
    "0",
    "false",
    "no",
}
AUTONOMI_NETWORK_ERROR_HINT = (
    "The Autonomi gateway is running, but its live-network write path is not "
    "connected to enough peers. Check the antd logs for bootstrap failures and "
    "verify this Docker host can reach the current Autonomi 2.0 bootstrap "
    "peers, or set PROD_AUTONOMI_PEERS to known-good Autonomi 2.0 peers."
)
AUTONOMI_NETWORK_ERROR_MARKERS = (
    "NETWORK_ERROR",
    "Found 0 peers",
    "need 7",
    "no peers connected",
    "Failed to connect to any bootstrap",
)


def _format_antd_error(exc: AntdError) -> str:
    message = str(exc)
    if any(marker in message for marker in AUTONOMI_NETWORK_ERROR_MARKERS):
        return f"{message}. {AUTONOMI_NETWORK_ERROR_HINT}"
    return message


def _parse_allowed_origins(raw_origins: str) -> list[str]:
    origins: list[str] = []
    for raw_origin in raw_origins.split(","):
        origin = raw_origin.strip()
        if not origin:
            continue
        if origin == "*":
            raise RuntimeError("CORS_ALLOWED_ORIGINS must list explicit origins, not '*'.")
        parsed = urlparse(origin)
        if (
            parsed.scheme not in {"http", "https"}
            or not parsed.netloc
            or parsed.params
            or parsed.query
            or parsed.fragment
            or parsed.path not in {"", "/"}
        ):
            raise RuntimeError(
                "CORS_ALLOWED_ORIGINS entries must be origins like "
                "'https://example.com' with no path, query, or wildcard."
            )
        origins.append(f"{parsed.scheme}://{parsed.netloc}")
    return origins


PRODUCTION_ENV_NAMES = {"prod", "production"}
UNSAFE_ADMIN_AUTH_VALUES = {
    "",
    "admin",
    "administrator",
    "changeme",
    "change-me",
    "change_me",
    "default",
    "password",
    "please-change-me",
    "replace-me",
    "secret",
    "test",
    "test-secret",
}


def _is_production_environment() -> bool:
    return any(
        os.environ.get(name, "").strip().lower() in PRODUCTION_ENV_NAMES
        for name in ("APP_ENV", "ENVIRONMENT")
    )


def _parse_admin_auth_ttl_hours(raw_ttl: str) -> int:
    try:
        ttl_hours = int(raw_ttl)
    except ValueError as exc:
        raise RuntimeError("ADMIN_AUTH_TTL_HOURS must be an integer") from exc
    if ttl_hours <= 0:
        raise RuntimeError("ADMIN_AUTH_TTL_HOURS must be greater than zero")
    return ttl_hours


def _is_unsafe_admin_auth_value(value: str) -> bool:
    normalized = value.strip().lower()
    if normalized in UNSAFE_ADMIN_AUTH_VALUES:
        return True
    return any(
        placeholder in normalized
        for placeholder in (
            "change-me",
            "change_me",
            "changeme",
            "change-this",
            "change_this",
            "changethis",
            "replace-me",
            "replace_me",
            "replace-this",
            "replace_this",
        )
    )


def _validate_admin_auth_config(
    username: str,
    password: str,
    secret: str,
    ttl_hours: int,
) -> None:
    if ttl_hours <= 0:
        raise RuntimeError("ADMIN_AUTH_TTL_HOURS must be greater than zero")

    if not _is_production_environment():
        return

    unsafe_fields = [
        name
        for name, value in (
            ("ADMIN_USERNAME", username),
            ("ADMIN_PASSWORD", password),
            ("ADMIN_AUTH_SECRET", secret),
        )
        if _is_unsafe_admin_auth_value(value)
    ]
    if unsafe_fields:
        raise RuntimeError(
            "Unsafe admin auth configuration for production: "
            + ", ".join(unsafe_fields)
            + " must not use default, weak, or change-me values"
        )

    if secrets.compare_digest(secret, password):
        raise RuntimeError(
            "Unsafe admin auth configuration for production: "
            "ADMIN_AUTH_SECRET must not equal ADMIN_PASSWORD"
        )

    if len(password) < 12:
        raise RuntimeError(
            "Unsafe admin auth configuration for production: "
            "ADMIN_PASSWORD must be at least 12 characters"
        )

    if len(secret) < 32:
        raise RuntimeError(
            "Unsafe admin auth configuration for production: "
            "ADMIN_AUTH_SECRET must be at least 32 characters"
        )


CORS_ALLOWED_ORIGINS = _parse_allowed_origins(
    os.environ.get("CORS_ALLOWED_ORIGINS", "http://localhost,http://127.0.0.1")
)
UPLOAD_TEMP_DIR = Path(os.environ.get("UPLOAD_TEMP_DIR", "/tmp/video_uploads"))
UPLOAD_TEMP_DIR.mkdir(parents=True, exist_ok=True)
UPLOAD_MAX_FILE_BYTES = int(
    os.environ.get("UPLOAD_MAX_FILE_BYTES", str(20 * 1024 * 1024 * 1024))
)
UPLOAD_MAX_DURATION_SECONDS = float(
    os.environ.get("UPLOAD_MAX_DURATION_SECONDS", str(4 * 60 * 60))
)
UPLOAD_MAX_SOURCE_PIXELS = int(
    os.environ.get("UPLOAD_MAX_SOURCE_PIXELS", str(7680 * 4320))
)
UPLOAD_MAX_SOURCE_LONG_EDGE = int(os.environ.get("UPLOAD_MAX_SOURCE_LONG_EDGE", "7680"))
UPLOAD_MIN_FREE_BYTES = int(
    os.environ.get("UPLOAD_MIN_FREE_BYTES", str(5 * 1024 * 1024 * 1024))
)
UPLOAD_MAX_CONCURRENT_SAVES = int(os.environ.get("UPLOAD_MAX_CONCURRENT_SAVES", "2"))
UPLOAD_FFPROBE_TIMEOUT_SECONDS = float(os.environ.get("UPLOAD_FFPROBE_TIMEOUT_SECONDS", "30"))
CATALOG_STATE_PATH = Path(os.environ.get("CATALOG_STATE_PATH", "/tmp/video_catalog/catalog.json"))
CATALOG_BOOTSTRAP_ADDRESS = os.environ.get("CATALOG_ADDRESS", "").strip()
CATALOG_CONTENT_TYPE = "application/vnd.autonomi.video.catalog+json;v=1"
VIDEO_MANIFEST_CONTENT_TYPE = "application/vnd.autonomi.video.manifest+json;v=1"
ADMIN_USERNAME = os.environ.get("ADMIN_USERNAME", "admin")
ADMIN_PASSWORD = os.environ.get("ADMIN_PASSWORD", "admin")
ADMIN_AUTH_SECRET = os.environ.get("ADMIN_AUTH_SECRET") or ADMIN_PASSWORD
ADMIN_AUTH_ALGORITHM = "HS256"
ADMIN_AUTH_TTL_HOURS = _parse_admin_auth_ttl_hours(
    os.environ.get("ADMIN_AUTH_TTL_HOURS", "12")
)
_validate_admin_auth_config(
    ADMIN_USERNAME,
    ADMIN_PASSWORD,
    ADMIN_AUTH_SECRET,
    ADMIN_AUTH_TTL_HOURS,
)

# Resolution presets: name -> (width, height, video_kbps, audio_kbps)
SUPPORTED_RESOLUTIONS = [
    "8k",
    "4k",
    "1440p",
    "1080p",
    "720p",
    "540p",
    "480p",
    "360p",
    "240p",
    "144p",
]
RESOLUTION_PRESETS: dict[str, tuple[int, int, int, int]] = {
    "8k":    (7680, 4320, 45000, 320),
    "4k":    (3840, 2160, 16000, 256),
    "1440p": (2560, 1440, 8000, 192),
    "1080p": (1920, 1080, 5000, 192),
    "720p":  (1280, 720,  2500, 128),
    "540p":  (960,  540,  1600, 128),
    "480p":  (854,  480,  1000, 128),
    "360p":  (640,  360,   500, 96),
    "240p":  (426,  240,   300, 64),
    "144p":  (256,  144,   150, 48),
}

VideoDimensions = tuple[int, int]
UploadMediaMetadata = tuple[float, VideoDimensions]

# Keep media chunks comfortably below Autonomi's multi-MB chunk boundary for
# reliable local-devnet storage and playback. FFmpeg is configured below to
# force keyframes at this cadence so these are real segment boundaries.
HLS_SEGMENT_DURATION = float(os.environ.get("HLS_SEGMENT_DURATION", "1"))
FFMPEG_THREADS = int(os.environ.get("FFMPEG_THREADS", "2"))
FFMPEG_FILTER_THREADS = int(os.environ.get("FFMPEG_FILTER_THREADS", "1"))
UPLOAD_QUOTE_TRANSCODED_OVERHEAD = float(
    os.environ.get("UPLOAD_QUOTE_TRANSCODED_OVERHEAD", "1.08")
)
UPLOAD_QUOTE_MAX_SAMPLE_BYTES = int(
    os.environ.get("UPLOAD_QUOTE_MAX_SAMPLE_BYTES", str(16 * 1024 * 1024))
)
FINAL_QUOTE_APPROVAL_TTL_SECONDS = int(
    os.environ.get("FINAL_QUOTE_APPROVAL_TTL_SECONDS", str(4 * 60 * 60))
)
APPROVAL_CLEANUP_INTERVAL_SECONDS = int(
    os.environ.get("APPROVAL_CLEANUP_INTERVAL_SECONDS", "300")
)
VALID_PAYMENT_MODES = {"auto", "merkle", "single"}
STATUS_PENDING = "pending"
STATUS_PROCESSING = "processing"
STATUS_AWAITING_APPROVAL = "awaiting_approval"
STATUS_UPLOADING = "uploading"
STATUS_READY = "ready"
STATUS_EXPIRED = "expired"
STATUS_ERROR = "error"
RECOVERABLE_PROCESSING_STATUSES = {STATUS_PENDING, STATUS_PROCESSING}
RECOVERABLE_UPLOAD_STATUSES = {STATUS_UPLOADING}
ACTIVE_RECOVERABLE_STATUSES = RECOVERABLE_PROCESSING_STATUSES | RECOVERABLE_UPLOAD_STATUSES

if ANTD_PAYMENT_MODE not in VALID_PAYMENT_MODES:
    raise RuntimeError(
        f"Invalid ANTD_PAYMENT_MODE={ANTD_PAYMENT_MODE!r}; "
        f"choose one of {sorted(VALID_PAYMENT_MODES)}"
    )

if HLS_SEGMENT_DURATION <= 0:
    raise RuntimeError("HLS_SEGMENT_DURATION must be greater than zero")

if FFMPEG_THREADS < 1:
    raise RuntimeError("FFMPEG_THREADS must be at least 1")

if FFMPEG_FILTER_THREADS < 1:
    raise RuntimeError("FFMPEG_FILTER_THREADS must be at least 1")

if UPLOAD_QUOTE_TRANSCODED_OVERHEAD < 1:
    raise RuntimeError("UPLOAD_QUOTE_TRANSCODED_OVERHEAD must be at least 1")

if UPLOAD_QUOTE_MAX_SAMPLE_BYTES < 1:
    raise RuntimeError("UPLOAD_QUOTE_MAX_SAMPLE_BYTES must be at least 1")

if ANTD_UPLOAD_RETRIES < 1:
    raise RuntimeError("ANTD_UPLOAD_RETRIES must be at least 1")

if ANTD_UPLOAD_TIMEOUT_SECONDS <= 0:
    raise RuntimeError("ANTD_UPLOAD_TIMEOUT_SECONDS must be greater than zero")

if FINAL_QUOTE_APPROVAL_TTL_SECONDS <= 0:
    raise RuntimeError("FINAL_QUOTE_APPROVAL_TTL_SECONDS must be greater than zero")

if APPROVAL_CLEANUP_INTERVAL_SECONDS <= 0:
    raise RuntimeError("APPROVAL_CLEANUP_INTERVAL_SECONDS must be greater than zero")

if UPLOAD_MAX_FILE_BYTES <= 0:
    raise RuntimeError("UPLOAD_MAX_FILE_BYTES must be greater than zero")

if UPLOAD_MAX_DURATION_SECONDS <= 0:
    raise RuntimeError("UPLOAD_MAX_DURATION_SECONDS must be greater than zero")

if UPLOAD_MAX_SOURCE_PIXELS <= 0:
    raise RuntimeError("UPLOAD_MAX_SOURCE_PIXELS must be greater than zero")

if UPLOAD_MAX_SOURCE_LONG_EDGE <= 0:
    raise RuntimeError("UPLOAD_MAX_SOURCE_LONG_EDGE must be greater than zero")

if UPLOAD_MIN_FREE_BYTES < 0:
    raise RuntimeError("UPLOAD_MIN_FREE_BYTES cannot be negative")

if UPLOAD_MAX_CONCURRENT_SAVES < 1:
    raise RuntimeError("UPLOAD_MAX_CONCURRENT_SAVES must be at least 1")

if UPLOAD_FFPROBE_TIMEOUT_SECONDS <= 0:
    raise RuntimeError("UPLOAD_FFPROBE_TIMEOUT_SECONDS must be greater than zero")

logging.basicConfig(level=logging.INFO)
log = logging.getLogger(__name__)
catalog_lock = asyncio.Lock()
upload_save_semaphore = asyncio.Semaphore(UPLOAD_MAX_CONCURRENT_SAVES)
active_job_tasks: dict[str, asyncio.Task] = {}

# ── Database pool ─────────────────────────────────────────────────────────────

pool: asyncpg.Pool | None = None


@asynccontextmanager
async def lifespan(app: FastAPI):
    global pool
    pool = await asyncpg.create_pool(DB_DSN, min_size=2, max_size=10)
    await _ensure_schema()
    _ensure_catalog_state_dir()
    await _ensure_autonomi_ready()
    await _cleanup_expired_approvals()
    await _recover_interrupted_jobs()
    cleanup_task = asyncio.create_task(_approval_cleanup_loop())
    try:
        yield
    finally:
        cleanup_task.cancel()
        for task in list(active_job_tasks.values()):
            task.cancel()
        try:
            await cleanup_task
        except asyncio.CancelledError:
            pass
        if active_job_tasks:
            await asyncio.gather(*active_job_tasks.values(), return_exceptions=True)
        await pool.close()


async def _ensure_schema():
    async with pool.acquire() as conn:
        await conn.execute("""
            CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

            CREATE TABLE IF NOT EXISTS videos (
                id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
                title TEXT NOT NULL,
                original_filename TEXT NOT NULL,
                description TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                manifest_address TEXT,
                catalog_address TEXT,
                error_message TEXT,
                job_dir TEXT,
                job_source_path TEXT,
                requested_resolutions JSONB,
                final_quote JSONB,
                final_quote_created_at TIMESTAMPTZ,
                approval_expires_at TIMESTAMPTZ,
                is_public BOOLEAN NOT NULL DEFAULT FALSE,
                show_original_filename BOOLEAN NOT NULL DEFAULT FALSE,
                show_manifest_address BOOLEAN NOT NULL DEFAULT FALSE,
                created_at TIMESTAMPTZ DEFAULT NOW(),
                updated_at TIMESTAMPTZ DEFAULT NOW(),
                user_id TEXT
            );

            ALTER TABLE videos
                ADD COLUMN IF NOT EXISTS manifest_address TEXT,
                ADD COLUMN IF NOT EXISTS catalog_address TEXT,
                ADD COLUMN IF NOT EXISTS error_message TEXT,
                ADD COLUMN IF NOT EXISTS job_dir TEXT,
                ADD COLUMN IF NOT EXISTS job_source_path TEXT,
                ADD COLUMN IF NOT EXISTS requested_resolutions JSONB,
                ADD COLUMN IF NOT EXISTS final_quote JSONB,
                ADD COLUMN IF NOT EXISTS final_quote_created_at TIMESTAMPTZ,
                ADD COLUMN IF NOT EXISTS approval_expires_at TIMESTAMPTZ,
                ADD COLUMN IF NOT EXISTS is_public BOOLEAN NOT NULL DEFAULT FALSE,
                ADD COLUMN IF NOT EXISTS show_original_filename BOOLEAN NOT NULL DEFAULT FALSE,
                ADD COLUMN IF NOT EXISTS show_manifest_address BOOLEAN NOT NULL DEFAULT FALSE;

            CREATE TABLE IF NOT EXISTS video_variants (
                id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
                video_id UUID NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
                resolution TEXT NOT NULL,
                width INTEGER NOT NULL,
                height INTEGER NOT NULL,
                video_bitrate INTEGER NOT NULL,
                audio_bitrate INTEGER NOT NULL,
                segment_duration FLOAT NOT NULL DEFAULT 10.0,
                total_duration FLOAT,
                segment_count INTEGER,
                created_at TIMESTAMPTZ DEFAULT NOW()
            );

            CREATE TABLE IF NOT EXISTS video_segments (
                id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
                variant_id UUID NOT NULL REFERENCES video_variants(id) ON DELETE CASCADE,
                segment_index INTEGER NOT NULL,
                autonomi_address TEXT,
                autonomi_cost_atto TEXT,
                autonomi_payment_mode TEXT,
                duration FLOAT NOT NULL DEFAULT 10.0,
                byte_size BIGINT,
                local_path TEXT,
                created_at TIMESTAMPTZ DEFAULT NOW(),
                UNIQUE (variant_id, segment_index)
            );

            ALTER TABLE video_segments
                ADD COLUMN IF NOT EXISTS autonomi_cost_atto TEXT,
                ADD COLUMN IF NOT EXISTS autonomi_payment_mode TEXT,
                ADD COLUMN IF NOT EXISTS local_path TEXT;

            ALTER TABLE video_segments
                ALTER COLUMN autonomi_address DROP NOT NULL;

            CREATE INDEX IF NOT EXISTS idx_videos_status ON videos(status);
            CREATE INDEX IF NOT EXISTS idx_videos_is_public ON videos(is_public);
            CREATE INDEX IF NOT EXISTS idx_variants_video ON video_variants(video_id);
            CREATE INDEX IF NOT EXISTS idx_segments_variant ON video_segments(variant_id);
        """)


async def _ensure_autonomi_ready():
    """Fail fast if antd is unavailable or cannot write with the configured wallet."""
    client = AsyncAntdClient(base_url=ANTD_URL, timeout=300)
    try:
        status = await client.health()
        log.info("Connected to antd network=%s ok=%s", status.network, status.ok)
        if not status.ok:
            raise RuntimeError("antd health check returned not ok")

        wallet = await client.wallet_address()
        balance = await client.wallet_balance()
        log.info(
            "Autonomi wallet %s balance=%s gas=%s",
            wallet.address,
            balance.balance,
            balance.gas_balance,
        )

        if ANTD_APPROVE_ON_STARTUP:
            approved = await client.wallet_approve()
            log.info("Autonomi wallet spend approval ready=%s", approved)
    except AntdError as exc:
        raise RuntimeError(
            f"Autonomi daemon is not ready at {ANTD_URL}: {_format_antd_error(exc)}"
        ) from exc
    finally:
        await client.close()


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _ensure_catalog_state_dir():
    CATALOG_STATE_PATH.parent.mkdir(parents=True, exist_ok=True)


def _read_catalog_address() -> str | None:
    if CATALOG_STATE_PATH.exists():
        try:
            state = json.loads(CATALOG_STATE_PATH.read_text())
            address = str(state.get("catalog_address", "")).strip()
            if address:
                return address
        except (OSError, json.JSONDecodeError):
            log.warning("Could not read catalog state from %s", CATALOG_STATE_PATH)

    return CATALOG_BOOTSTRAP_ADDRESS or None


def _write_catalog_address(address: str):
    _ensure_catalog_state_dir()
    tmp_path = CATALOG_STATE_PATH.with_suffix(".tmp")
    tmp_path.write_text(json.dumps({
        "catalog_address": address,
        "updated_at": _now_iso(),
        "note": "This is only a bookmark to the latest network-hosted catalog snapshot.",
    }, indent=2))
    tmp_path.replace(CATALOG_STATE_PATH)


async def _approval_cleanup_loop():
    while True:
        await asyncio.sleep(APPROVAL_CLEANUP_INTERVAL_SECONDS)
        try:
            await _cleanup_expired_approvals()
        except Exception as exc:
            log.warning("Approval cleanup failed: %s", exc)


def _track_job_task(task_key: str, coro):
    existing = active_job_tasks.get(task_key)
    if existing and not existing.done():
        coro.close()
        return

    task = asyncio.create_task(coro)
    active_job_tasks[task_key] = task

    def _forget_task(done_task: asyncio.Task):
        active_job_tasks.pop(task_key, None)
        if done_task.cancelled():
            return
        exc = done_task.exception()
        if exc:
            log.error(
                "Background job %s crashed outside its handler",
                task_key,
                exc_info=(type(exc), exc, exc.__traceback__),
            )

    task.add_done_callback(_forget_task)


def _schedule_processing_job(
    video_id: str,
    src_path: Path,
    resolutions: list[str],
    job_dir: Path,
    *,
    reset_existing: bool = False,
):
    _track_job_task(
        f"process:{video_id}",
        _process_video(video_id, src_path, resolutions, job_dir, reset_existing=reset_existing),
    )


def _schedule_upload_job(video_id: str):
    _track_job_task(f"upload:{video_id}", _upload_approved_video(video_id))


def _decode_requested_resolutions(value) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except json.JSONDecodeError:
            value = [part.strip() for part in value.split(",")]
    if not isinstance(value, list):
        return []
    return [str(item) for item in value if str(item) in RESOLUTION_PRESETS]


def _recover_source_path(job_dir: Path | None, job_source_path: str | None) -> Path | None:
    if job_source_path:
        source_path = Path(job_source_path)
        if source_path.exists():
            return source_path

    if job_dir and job_dir.exists():
        matches = sorted(job_dir.glob("original_*"))
        for match in matches:
            if match.is_file():
                return match
    return None


async def _recover_interrupted_jobs():
    async with pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT id, status, job_dir, job_source_path, requested_resolutions
            FROM videos
            WHERE status IN ('pending', 'processing', 'uploading')
            ORDER BY created_at
            """
        )

    recovered_processing = 0
    recovered_uploads = 0
    for row in rows:
        video_id = str(row["id"])
        status = row["status"]
        job_dir = Path(row["job_dir"]) if row["job_dir"] else None

        if status in RECOVERABLE_PROCESSING_STATUSES:
            resolutions = _decode_requested_resolutions(row["requested_resolutions"])
            source_path = _recover_source_path(job_dir, row["job_source_path"])
            if not job_dir or not job_dir.exists() or not source_path or not resolutions:
                await _set_status(
                    video_id,
                    STATUS_ERROR,
                    "Interrupted processing job could not be recovered because its "
                    "source file or requested resolutions were missing.",
                )
                log.warning("Could not recover interrupted processing job %s", video_id)
                continue

            _schedule_processing_job(
                video_id,
                source_path,
                resolutions,
                job_dir,
                reset_existing=True,
            )
            recovered_processing += 1
            continue

        _schedule_upload_job(video_id)
        recovered_uploads += 1

    if recovered_processing or recovered_uploads:
        log.info(
            "Recovered interrupted jobs: processing=%d uploading=%d",
            recovered_processing,
            recovered_uploads,
        )


async def _cleanup_expired_approvals():
    """Delete local transcoded files for approval jobs that have gone stale."""
    async with pool.acquire() as conn:
        rows = await conn.fetch(
            """
            UPDATE videos
            SET status='expired',
                error_message='Final quote approval window expired; local files were deleted.',
                updated_at=NOW()
            WHERE status='awaiting_approval'
              AND approval_expires_at IS NOT NULL
              AND approval_expires_at <= NOW()
            RETURNING id, job_dir
            """
        )

    for row in rows:
        job_dir = row["job_dir"]
        if job_dir:
            shutil.rmtree(job_dir, ignore_errors=True)
        log.info("Expired awaiting approval video %s and removed local files", row["id"])


def _empty_catalog() -> dict:
    return {
        "schema_version": 1,
        "content_type": CATALOG_CONTENT_TYPE,
        "updated_at": _now_iso(),
        "videos": [],
    }


async def _load_catalog(client: AsyncAntdClient) -> tuple[dict, str | None]:
    address = _read_catalog_address()
    if not address:
        return _empty_catalog(), None

    try:
        data = await client.data_get_public(address)
        catalog = json.loads(data.decode("utf-8"))
        if not isinstance(catalog.get("videos"), list):
            catalog["videos"] = []
        return catalog, address
    except Exception as exc:
        log.warning("Could not load Autonomi catalog %s: %s", address, exc)
        return _empty_catalog(), address


async def _store_json_public(client: AsyncAntdClient, payload: dict) -> str:
    data = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    result = await _put_public_verified(client, data, label="json document")
    return result.address


async def _put_public_verified(client: AsyncAntdClient, data: bytes, label: str):
    """Store bytes and optionally verify the address can reconstruct them."""
    last_error: Exception | None = None

    for attempt in range(1, ANTD_UPLOAD_RETRIES + 1):
        try:
            log.info(
                "Uploading %s (%d bytes), attempt %d/%d",
                label,
                len(data),
                attempt,
                ANTD_UPLOAD_RETRIES,
            )
            result = await asyncio.wait_for(
                client.data_put_public(data, payment_mode=ANTD_PAYMENT_MODE),
                timeout=ANTD_UPLOAD_TIMEOUT_SECONDS,
            )
            if ANTD_UPLOAD_VERIFY:
                retrieved = await asyncio.wait_for(
                    client.data_get_public(result.address),
                    timeout=ANTD_UPLOAD_TIMEOUT_SECONDS,
                )
                if retrieved != data:
                    raise RuntimeError(
                        f"Autonomi verification mismatch for {label}: "
                        f"stored {len(data)} bytes, retrieved {len(retrieved)} bytes"
                    )
            return result
        except Exception as exc:
            last_error = exc
            if attempt == ANTD_UPLOAD_RETRIES:
                break
            delay = min(2 ** (attempt - 1), 8)
            log.warning(
                "Autonomi upload verification failed for %s on attempt %d/%d: %s; retrying in %ss",
                label,
                attempt,
                ANTD_UPLOAD_RETRIES,
                exc,
                delay,
            )
            await asyncio.sleep(delay)

    raise RuntimeError(
        f"Autonomi upload failed verification for {label} "
        f"after {ANTD_UPLOAD_RETRIES} attempt(s): {last_error}"
    )


def _parse_cost_value(value: str | None) -> int:
    try:
        return int(value or "0")
    except (TypeError, ValueError):
        return 0


def _decode_jsonb(value) -> dict | None:
    if value is None:
        return None
    if isinstance(value, dict):
        return value
    try:
        return json.loads(value)
    except (TypeError, json.JSONDecodeError):
        return None


def _create_admin_token() -> tuple[str, datetime]:
    expires_at = datetime.now(timezone.utc) + timedelta(hours=ADMIN_AUTH_TTL_HOURS)
    token = jwt.encode(
        {"sub": ADMIN_USERNAME, "exp": expires_at},
        ADMIN_AUTH_SECRET,
        algorithm=ADMIN_AUTH_ALGORITHM,
    )
    return token, expires_at


def require_admin(authorization: str | None = Header(None)) -> str:
    if not authorization or not authorization.lower().startswith("bearer "):
        raise HTTPException(401, "Login required", headers={"WWW-Authenticate": "Bearer"})

    token = authorization.split(" ", 1)[1].strip()
    try:
        payload = jwt.decode(token, ADMIN_AUTH_SECRET, algorithms=[ADMIN_AUTH_ALGORITHM])
    except JWTError as exc:
        raise HTTPException(401, "Invalid or expired login", headers={"WWW-Authenticate": "Bearer"}) from exc

    username = payload.get("sub")
    if username != ADMIN_USERNAME:
        raise HTTPException(401, "Invalid or expired login", headers={"WWW-Authenticate": "Bearer"})
    return username


def _ceil_ratio(value: int, numerator: int, denominator: int) -> int:
    if value <= 0 or numerator <= 0:
        return 0
    return (value * numerator + denominator - 1) // denominator


def _format_bytes(byte_count: int) -> str:
    value = float(byte_count)
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if value < 1024 or unit == "TiB":
            return f"{value:.1f} {unit}" if unit != "B" else f"{byte_count} B"
        value /= 1024
    return f"{byte_count} B"


def _format_duration(seconds: float) -> str:
    minutes, sec = divmod(int(math.ceil(seconds)), 60)
    hours, minute = divmod(minutes, 60)
    if hours:
        return f"{hours}h {minute}m {sec}s"
    if minute:
        return f"{minute}m {sec}s"
    return f"{sec}s"


def _sanitize_upload_filename(filename: str | None) -> str:
    basename = Path(filename or "upload").name
    stem = Path(basename).stem
    suffix = Path(basename).suffix.lower()
    safe_stem = re.sub(r"[^A-Za-z0-9._-]+", "_", stem).strip("._-")
    safe_suffix = re.sub(r"[^A-Za-z0-9.]+", "", suffix)[:16]

    if not safe_stem:
        safe_stem = "upload"

    max_stem_length = max(1, 128 - len(safe_suffix))
    return f"{safe_stem[:max_stem_length]}{safe_suffix}"


def _ensure_upload_disk_space(additional_bytes: int = 0):
    free_bytes = shutil.disk_usage(UPLOAD_TEMP_DIR).free
    required_free = UPLOAD_MIN_FREE_BYTES + max(0, additional_bytes)
    if free_bytes < required_free:
        raise HTTPException(
            507,
            "Not enough upload disk space "
            f"(free={_format_bytes(free_bytes)}, required={_format_bytes(required_free)})",
        )


def _enforce_upload_media_limits(duration_seconds: float, dimensions: VideoDimensions):
    width, height = dimensions
    if duration_seconds > UPLOAD_MAX_DURATION_SECONDS:
        raise HTTPException(
            413,
            "Video duration exceeds upload limit "
            f"({_format_duration(duration_seconds)} > {_format_duration(UPLOAD_MAX_DURATION_SECONDS)})",
        )

    pixel_count = width * height
    long_edge = max(width, height)
    if long_edge > UPLOAD_MAX_SOURCE_LONG_EDGE or pixel_count > UPLOAD_MAX_SOURCE_PIXELS:
        max_megapixels = UPLOAD_MAX_SOURCE_PIXELS / 1_000_000
        raise HTTPException(
            413,
            "Video resolution exceeds upload limit "
            f"({width}x{height}; max long edge {UPLOAD_MAX_SOURCE_LONG_EDGE}px "
            f"and {max_megapixels:.1f} MP)",
        )


async def _save_upload_stream(file: UploadFile, destination: Path) -> int:
    bytes_written = 0
    try:
        with open(destination, "wb") as f_out:
            while chunk := await file.read(1024 * 1024):
                next_size = bytes_written + len(chunk)
                if next_size > UPLOAD_MAX_FILE_BYTES:
                    raise HTTPException(
                        413,
                        "Upload exceeds max file size "
                        f"({_format_bytes(UPLOAD_MAX_FILE_BYTES)})",
                    )
                _ensure_upload_disk_space(len(chunk))
                f_out.write(chunk)
                bytes_written = next_size
    except Exception:
        destination.unlink(missing_ok=True)
        raise

    if bytes_written == 0:
        destination.unlink(missing_ok=True)
        raise HTTPException(400, "Uploaded file is empty")
    return bytes_written


def _estimate_transcoded_bytes(seconds: float, video_kbps: int, audio_kbps: int) -> int:
    if seconds <= 0:
        return 0
    bitrate_bps = (video_kbps + audio_kbps) * 1000
    media_bytes = seconds * bitrate_bps / 8
    return max(1, math.ceil(media_bytes * UPLOAD_QUOTE_TRANSCODED_OVERHEAD))


async def _quote_data_size(
    client: AsyncAntdClient,
    byte_size: int,
    cache: dict[int, dict],
) -> dict:
    """Ask antd for a storage quote for byte_size, sampling if unusually large."""
    if byte_size <= 0:
        return {
            "sampled": False,
            "sample_bytes": 0,
            "storage_cost_atto": 0,
            "estimated_gas_cost_wei": 0,
            "chunk_count": 0,
            "payment_mode": ANTD_PAYMENT_MODE,
        }

    sample_bytes = min(byte_size, UPLOAD_QUOTE_MAX_SAMPLE_BYTES)
    if sample_bytes not in cache:
        estimate = await client.data_cost(os.urandom(sample_bytes))
        cache[sample_bytes] = {
            "storage_cost_atto": _parse_cost_value(estimate.cost),
            "estimated_gas_cost_wei": _parse_cost_value(estimate.estimated_gas_cost_wei),
            "chunk_count": estimate.chunk_count,
            "payment_mode": estimate.payment_mode or ANTD_PAYMENT_MODE,
        }

    quoted = cache[sample_bytes]
    if sample_bytes == byte_size:
        return {
            **quoted,
            "sampled": False,
            "sample_bytes": sample_bytes,
        }

    return {
        "sampled": True,
        "sample_bytes": sample_bytes,
        "storage_cost_atto": _ceil_ratio(quoted["storage_cost_atto"], byte_size, sample_bytes),
        "estimated_gas_cost_wei": _ceil_ratio(quoted["estimated_gas_cost_wei"], byte_size, sample_bytes),
        "chunk_count": max(1, _ceil_ratio(quoted["chunk_count"], byte_size, sample_bytes)),
        "payment_mode": quoted["payment_mode"],
    }


def _target_dimensions_for_source(
    preset_width: int,
    preset_height: int,
    source_dimensions: VideoDimensions | None,
) -> VideoDimensions:
    """Preserve source aspect ratio at the selected quality tier."""
    short_edge = min(preset_width, preset_height)
    if not source_dimensions:
        return preset_width, preset_height

    source_width, source_height = source_dimensions
    if source_height > source_width:
        return _fit_within_source(
            short_edge,
            _even_floor(short_edge * source_height / source_width),
            source_width,
            source_height,
        )
    if source_width > source_height:
        return _fit_within_source(
            _even_floor(short_edge * source_width / source_height),
            short_edge,
            source_width,
            source_height,
        )
    return _fit_within_source(short_edge, short_edge, source_width, source_height)


def _even_floor(value: float) -> int:
    floored = max(2, math.floor(value))
    return max(2, floored - (floored % 2))


def _fit_within_source(
    width: int,
    height: int,
    source_width: int,
    source_height: int,
) -> VideoDimensions:
    if width <= source_width and height <= source_height:
        return width, height
    scale = min(source_width / width, source_height / height, 1)
    return _even_floor(width * scale), _even_floor(height * scale)


def _target_video_bitrate_kbps(
    base_video_kbps: int,
    preset_width: int,
    preset_height: int,
    width: int,
    height: int,
) -> int:
    base_pixels = preset_width * preset_height
    if base_pixels <= 0:
        return base_video_kbps
    return max(64, round(base_video_kbps * ((width * height) / base_pixels)))


def _request_source_dimensions(request: "UploadQuoteRequest") -> VideoDimensions | None:
    if request.source_width is None and request.source_height is None:
        return None
    if request.source_width is None or request.source_height is None:
        raise HTTPException(400, "source_width and source_height must be provided together")
    if request.source_width <= 0 or request.source_height <= 0:
        raise HTTPException(400, "source_width and source_height must be greater than zero")
    return request.source_width, request.source_height


async def _build_upload_quote(
    duration_seconds: float,
    resolutions: list[str],
    source_dimensions: VideoDimensions | None = None,
) -> dict:
    if duration_seconds <= 0:
        raise HTTPException(400, "duration_seconds must be greater than zero")
    if source_dimensions:
        _enforce_upload_media_limits(duration_seconds, source_dimensions)
    elif duration_seconds > UPLOAD_MAX_DURATION_SECONDS:
        raise HTTPException(
            413,
            "Video duration exceeds upload limit "
            f"({_format_duration(duration_seconds)} > {_format_duration(UPLOAD_MAX_DURATION_SECONDS)})",
        )

    selected = [resolution for resolution in resolutions if resolution in RESOLUTION_PRESETS]
    if not selected:
        raise HTTPException(400, f"No valid resolutions. Choose from: {SUPPORTED_RESOLUTIONS}")

    client = AsyncAntdClient(base_url=ANTD_URL, timeout=max(ANTD_UPLOAD_TIMEOUT_SECONDS, 60))
    quote_cache: dict[int, dict] = {}
    try:
        variants = []
        total_storage_cost = 0
        total_gas_cost = 0
        total_bytes = 0
        total_segments = 0
        any_sampled = False

        for resolution in selected:
            preset_width, preset_height, video_kbps, audio_kbps = RESOLUTION_PRESETS[resolution]
            width, height = _target_dimensions_for_source(
                preset_width,
                preset_height,
                source_dimensions,
            )
            video_kbps = _target_video_bitrate_kbps(
                video_kbps,
                preset_width,
                preset_height,
                width,
                height,
            )
            full_segments = int(duration_seconds // HLS_SEGMENT_DURATION)
            remainder = duration_seconds - (full_segments * HLS_SEGMENT_DURATION)
            if remainder < 0.01:
                remainder = 0

            segment_count = full_segments + (1 if remainder > 0 else 0)
            full_segment_bytes = _estimate_transcoded_bytes(
                min(HLS_SEGMENT_DURATION, duration_seconds),
                video_kbps,
                audio_kbps,
            )
            full_quote = await _quote_data_size(client, full_segment_bytes, quote_cache)

            variant_storage_cost = full_quote["storage_cost_atto"] * full_segments
            variant_gas_cost = full_quote["estimated_gas_cost_wei"] * full_segments
            variant_bytes = full_segment_bytes * full_segments
            variant_chunks = full_quote["chunk_count"] * full_segments
            any_sampled = any_sampled or full_quote["sampled"]

            if remainder > 0:
                final_segment_bytes = _estimate_transcoded_bytes(
                    remainder,
                    video_kbps,
                    audio_kbps,
                )
                final_quote = await _quote_data_size(client, final_segment_bytes, quote_cache)
                variant_storage_cost += final_quote["storage_cost_atto"]
                variant_gas_cost += final_quote["estimated_gas_cost_wei"]
                variant_bytes += final_segment_bytes
                variant_chunks += final_quote["chunk_count"]
                any_sampled = any_sampled or final_quote["sampled"]

            variants.append({
                "resolution": resolution,
                "width": width,
                "height": height,
                "segment_count": segment_count,
                "estimated_bytes": variant_bytes,
                "chunk_count": variant_chunks,
                "storage_cost_atto": str(variant_storage_cost),
                "estimated_gas_cost_wei": str(variant_gas_cost),
                "payment_mode": full_quote["payment_mode"],
            })
            total_storage_cost += variant_storage_cost
            total_gas_cost += variant_gas_cost
            total_bytes += variant_bytes
            total_segments += segment_count

        manifest_bytes = 4096 + (len(selected) * 1024) + (total_segments * 220)
        catalog_bytes = 2048 + (len(selected) * 512)
        metadata_quote = await _quote_data_size(client, manifest_bytes + catalog_bytes, quote_cache)
        total_storage_cost += metadata_quote["storage_cost_atto"]
        total_gas_cost += metadata_quote["estimated_gas_cost_wei"]
        total_bytes += manifest_bytes + catalog_bytes
        any_sampled = any_sampled or metadata_quote["sampled"]

        return {
            "duration_seconds": duration_seconds,
            "segment_duration": float(HLS_SEGMENT_DURATION),
            "payment_mode": ANTD_PAYMENT_MODE,
            "estimated_bytes": total_bytes,
            "segment_count": total_segments,
            "storage_cost_atto": str(total_storage_cost),
            "estimated_gas_cost_wei": str(total_gas_cost),
            "metadata_bytes": manifest_bytes + catalog_bytes,
            "sampled": any_sampled,
            "variants": variants,
        }
    except AntdError as exc:
        raise HTTPException(
            503,
            f"Could not get Autonomi price quote: {_format_antd_error(exc)}",
        ) from exc
    finally:
        await client.close()


async def _build_final_upload_quote(video_id: str) -> dict:
    """Quote the actual transcoded segment bytes that are waiting on disk."""
    async with pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT v.id AS variant_id, v.resolution, v.width, v.height, v.total_duration,
                   s.segment_index, s.local_path, s.byte_size
            FROM video_variants v
            JOIN video_segments s ON s.variant_id = v.id
            WHERE v.video_id=$1
            ORDER BY v.height DESC, s.segment_index
            """,
            video_id,
        )

    if not rows:
        raise RuntimeError("No transcoded segments were found for final quote")

    client = AsyncAntdClient(base_url=ANTD_URL, timeout=max(ANTD_UPLOAD_TIMEOUT_SECONDS, 60))
    quote_cache: dict[int, dict] = {}
    try:
        variants_by_id: dict[str, dict] = {}
        total_storage_cost = 0
        total_gas_cost = 0
        total_bytes = 0
        total_chunks = 0

        for row in rows:
            path = Path(row["local_path"] or "")
            if not path.exists():
                raise RuntimeError(f"Transcoded segment is missing from disk: {path}")

            data = path.read_bytes()
            estimate = await client.data_cost(data)
            storage_cost = _parse_cost_value(estimate.cost)
            gas_cost = _parse_cost_value(estimate.estimated_gas_cost_wei)
            chunk_count = int(estimate.chunk_count or 0)
            byte_size = len(data)

            variant_id = str(row["variant_id"])
            variant = variants_by_id.setdefault(
                variant_id,
                {
                    "resolution": row["resolution"],
                    "width": row["width"],
                    "height": row["height"],
                    "segment_count": 0,
                    "estimated_bytes": 0,
                    "actual_bytes": 0,
                    "chunk_count": 0,
                    "storage_cost_atto": 0,
                    "estimated_gas_cost_wei": 0,
                    "payment_mode": estimate.payment_mode or ANTD_PAYMENT_MODE,
                },
            )
            variant["segment_count"] += 1
            variant["estimated_bytes"] += byte_size
            variant["actual_bytes"] += byte_size
            variant["chunk_count"] += chunk_count
            variant["storage_cost_atto"] += storage_cost
            variant["estimated_gas_cost_wei"] += gas_cost

            total_storage_cost += storage_cost
            total_gas_cost += gas_cost
            total_bytes += byte_size
            total_chunks += chunk_count

        manifest_bytes = 4096 + (len(variants_by_id) * 1024) + (len(rows) * 220)
        catalog_bytes = 2048 + (len(variants_by_id) * 512)
        metadata_quote = await _quote_data_size(client, manifest_bytes + catalog_bytes, quote_cache)

        total_storage_cost += metadata_quote["storage_cost_atto"]
        total_gas_cost += metadata_quote["estimated_gas_cost_wei"]
        total_bytes += manifest_bytes + catalog_bytes
        total_chunks += metadata_quote["chunk_count"]

        variants = []
        for variant in variants_by_id.values():
            variants.append({
                **variant,
                "storage_cost_atto": str(variant["storage_cost_atto"]),
                "estimated_gas_cost_wei": str(variant["estimated_gas_cost_wei"]),
            })

        return {
            "quote_type": "final",
            "duration_seconds": max((float(row["total_duration"] or 0) for row in rows), default=0),
            "segment_duration": float(HLS_SEGMENT_DURATION),
            "payment_mode": ANTD_PAYMENT_MODE,
            "estimated_bytes": total_bytes,
            "actual_media_bytes": total_bytes - (manifest_bytes + catalog_bytes),
            "segment_count": len(rows),
            "chunk_count": total_chunks,
            "storage_cost_atto": str(total_storage_cost),
            "estimated_gas_cost_wei": str(total_gas_cost),
            "metadata_bytes": manifest_bytes + catalog_bytes,
            "sampled": metadata_quote["sampled"],
            "approval_ttl_seconds": FINAL_QUOTE_APPROVAL_TTL_SECONDS,
            "variants": variants,
        }
    except AntdError as exc:
        raise RuntimeError(
            f"Could not get final Autonomi price quote: {_format_antd_error(exc)}"
        ) from exc
    finally:
        await client.close()


def _video_catalog_entry(manifest: dict, manifest_address: str) -> dict:
    show_original_filename = bool(manifest.get("show_original_filename"))
    return {
        "id": manifest["id"],
        "title": manifest["title"],
        "original_filename": manifest.get("original_filename") if show_original_filename else None,
        "description": manifest.get("description"),
        "status": STATUS_READY,
        "created_at": manifest["created_at"],
        "updated_at": manifest["updated_at"],
        "manifest_address": manifest_address,
        "show_original_filename": show_original_filename,
        "show_manifest_address": bool(manifest.get("show_manifest_address")),
        "variants": [
            {
                "resolution": variant["resolution"],
                "width": variant["width"],
                "height": variant["height"],
                "segment_count": variant["segment_count"],
                "total_duration": variant.get("total_duration"),
            }
            for variant in manifest.get("variants", [])
        ],
    }


async def _publish_video_to_catalog(client: AsyncAntdClient, manifest: dict) -> tuple[str, str]:
    async with catalog_lock:
        await _validate_manifest_segments_retrievable(client, manifest)
        manifest_address = await _store_json_public(client, manifest)
        catalog, _ = await _load_catalog(client)
        catalog["schema_version"] = 1
        catalog["content_type"] = CATALOG_CONTENT_TYPE
        catalog["updated_at"] = _now_iso()
        catalog["videos"] = [
            video for video in catalog.get("videos", [])
            if video.get("id") != manifest["id"]
        ]
        catalog["videos"].insert(0, _video_catalog_entry(manifest, manifest_address))
        catalog_address = await _store_json_public(client, catalog)
        _write_catalog_address(catalog_address)
        return manifest_address, catalog_address


async def _validate_manifest_segments_retrievable(client: AsyncAntdClient, manifest: dict):
    """Refuse to publish catalog entries that point at missing segment data."""
    for variant in manifest.get("variants", []):
        resolution = variant.get("resolution", "unknown")
        for segment in variant.get("segments", []):
            address = segment.get("autonomi_address")
            if not address:
                raise HTTPException(
                    409,
                    f"Video segment {resolution}/{segment.get('segment_index')} has no Autonomi address",
                )
            try:
                await client.data_get_public(address)
            except Exception as exc:
                raise HTTPException(
                    409,
                    "Video segment data is no longer retrievable from Autonomi; "
                    "delete and re-upload the source video before publishing.",
                ) from exc


async def _remove_video_from_catalog(video_id: str) -> str | None:
    async with catalog_lock:
        client = AsyncAntdClient(
            base_url=ANTD_URL,
            timeout=max(ANTD_UPLOAD_TIMEOUT_SECONDS + 30, 60),
        )
        try:
            catalog, current_address = await _load_catalog(client)
            videos = [
                video for video in catalog.get("videos", [])
                if video.get("id") != video_id
            ]
            if len(videos) == len(catalog.get("videos", [])):
                return current_address

            catalog["videos"] = videos
            catalog["updated_at"] = _now_iso()
            catalog_address = await _store_json_public(client, catalog)
            _write_catalog_address(catalog_address)
            return catalog_address
        finally:
            await client.close()


async def _load_video_manifest(client: AsyncAntdClient, video_id: str) -> tuple[dict, str] | None:
    catalog, _ = await _load_catalog(client)
    for video in catalog.get("videos", []):
        if video.get("id") == video_id and video.get("manifest_address"):
            address = video["manifest_address"]
            data = await client.data_get_public(address)
            return json.loads(data.decode("utf-8")), address
    return None


def _public_entry_to_video_out(entry: dict, catalog_address: str | None) -> "VideoOut":
    show_original_filename = bool(entry.get("show_original_filename"))
    show_manifest_address = bool(entry.get("show_manifest_address"))
    return VideoOut(
        id=entry["id"],
        title=entry["title"],
        original_filename=entry.get("original_filename") if show_original_filename else None,
        description=entry.get("description"),
        status=entry.get("status", STATUS_READY),
        created_at=entry["created_at"],
        manifest_address=entry.get("manifest_address") if show_manifest_address else None,
        catalog_address=None,
        is_public=True,
        show_original_filename=show_original_filename,
        show_manifest_address=show_manifest_address,
        variants=[
            VariantOut(
                id=f"{entry['id']}:{variant['resolution']}",
                resolution=variant["resolution"],
                width=variant["width"],
                height=variant["height"],
                total_duration=variant.get("total_duration"),
                segment_count=variant.get("segment_count"),
                segments=[],
            )
            for variant in entry.get("variants", [])
        ],
    )


def _manifest_to_video_out(
    manifest: dict,
    manifest_address: str | None = None,
    *,
    public: bool = False,
) -> "VideoOut":
    show_original_filename = bool(manifest.get("show_original_filename"))
    show_manifest_address = bool(manifest.get("show_manifest_address"))
    return VideoOut(
        id=manifest["id"],
        title=manifest["title"],
        original_filename=manifest.get("original_filename") if (not public or show_original_filename) else None,
        description=manifest.get("description"),
        status=manifest.get("status", STATUS_READY),
        created_at=manifest["created_at"],
        manifest_address=(manifest_address or manifest.get("manifest_address")) if (not public or show_manifest_address) else None,
        catalog_address=_read_catalog_address() if not public else None,
        is_public=public,
        show_original_filename=show_original_filename,
        show_manifest_address=show_manifest_address,
        variants=[
            VariantOut(
                id=f"{manifest['id']}:{variant['resolution']}",
                resolution=variant["resolution"],
                width=variant["width"],
                height=variant["height"],
                total_duration=variant.get("total_duration"),
                segment_count=variant.get("segment_count"),
                segments=[] if public else [
                    SegmentOut(
                        segment_index=segment["segment_index"],
                        autonomi_address=segment["autonomi_address"],
                        duration=segment["duration"],
                    )
                    for segment in variant.get("segments", [])
                ],
            )
            for variant in manifest.get("variants", [])
        ],
    )


# ── App ───────────────────────────────────────────────────────────────────────

app = FastAPI(title="AutVid Admin", lifespan=lifespan)

app.add_middleware(
    CORSMiddleware,
    allow_origins=CORS_ALLOWED_ORIGINS,
    allow_methods=["GET", "POST", "PATCH", "DELETE", "OPTIONS"],
    allow_headers=["Accept", "Authorization", "Content-Type", "Range"],
)


# ── Pydantic models ───────────────────────────────────────────────────────────

class SegmentOut(BaseModel):
    segment_index: int
    autonomi_address: str | None = None
    duration: float


class VariantOut(BaseModel):
    id: str
    resolution: str
    width: int
    height: int
    total_duration: float | None
    segment_count: int | None
    segments: list[SegmentOut] = []


class VideoOut(BaseModel):
    id: str
    title: str
    original_filename: str | None = None
    description: str | None
    status: str
    created_at: str
    manifest_address: str | None = None
    catalog_address: str | None = None
    is_public: bool = False
    show_original_filename: bool = False
    show_manifest_address: bool = False
    error_message: str | None = None
    final_quote: dict | None = None
    final_quote_created_at: str | None = None
    approval_expires_at: str | None = None
    variants: list[VariantOut] = []


class LoginRequest(BaseModel):
    username: str
    password: str


class AuthTokenOut(BaseModel):
    access_token: str
    token_type: str = "bearer"
    expires_at: str
    username: str


class AdminMeOut(BaseModel):
    username: str


class VideoVisibilityUpdate(BaseModel):
    show_original_filename: bool
    show_manifest_address: bool


class VideoPublicationUpdate(BaseModel):
    is_public: bool


class UploadQuoteRequest(BaseModel):
    duration_seconds: float
    resolutions: list[str]
    source_width: int | None = None
    source_height: int | None = None


class UploadQuoteVariantOut(BaseModel):
    resolution: str
    width: int
    height: int
    segment_count: int
    estimated_bytes: int
    chunk_count: int
    storage_cost_atto: str
    estimated_gas_cost_wei: str
    payment_mode: str


class UploadQuoteOut(BaseModel):
    duration_seconds: float
    segment_duration: float
    payment_mode: str
    estimated_bytes: int
    segment_count: int
    storage_cost_atto: str
    estimated_gas_cost_wei: str
    metadata_bytes: int
    sampled: bool
    variants: list[UploadQuoteVariantOut]


async def _db_video_to_out(row, *, include_segments: bool = False) -> VideoOut:
    async with pool.acquire() as conn:
        variants_rows = await conn.fetch(
            """
            SELECT id, resolution, width, height, total_duration, segment_count
            FROM video_variants WHERE video_id=$1 ORDER BY height DESC
            """,
            str(row["id"]),
        )
        variants = []
        for v in variants_rows:
            segments = []
            if include_segments:
                seg_rows = await conn.fetch(
                    """
                    SELECT segment_index, autonomi_address, duration
                    FROM video_segments WHERE variant_id=$1 ORDER BY segment_index
                    """,
                    str(v["id"]),
                )
                segments = [
                    SegmentOut(
                        segment_index=s["segment_index"],
                        autonomi_address=s["autonomi_address"],
                        duration=s["duration"],
                    )
                    for s in seg_rows
                ]
            variants.append(VariantOut(
                id=str(v["id"]),
                resolution=v["resolution"],
                width=v["width"],
                height=v["height"],
                total_duration=v["total_duration"],
                segment_count=v["segment_count"],
                segments=segments,
            ))

    return VideoOut(
        id=str(row["id"]),
        title=row["title"],
        original_filename=row["original_filename"],
        description=row["description"],
        status=row["status"],
        created_at=str(row["created_at"]),
        manifest_address=row["manifest_address"],
        catalog_address=row["catalog_address"] or _read_catalog_address(),
        is_public=row["is_public"],
        show_original_filename=row["show_original_filename"],
        show_manifest_address=row["show_manifest_address"],
        error_message=row["error_message"],
        final_quote=_decode_jsonb(row["final_quote"]),
        final_quote_created_at=str(row["final_quote_created_at"]) if row["final_quote_created_at"] else None,
        approval_expires_at=str(row["approval_expires_at"]) if row["approval_expires_at"] else None,
        variants=variants,
    )


async def _get_db_video(video_id: str, *, include_segments: bool = False) -> VideoOut:
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            SELECT id, title, original_filename, description, status, created_at,
                   manifest_address, catalog_address, error_message, final_quote,
                   final_quote_created_at, approval_expires_at,
                   is_public, show_original_filename, show_manifest_address
            FROM videos WHERE id=$1
            """,
            video_id,
        )
    if not row:
        raise HTTPException(404, "Video not found")
    return await _db_video_to_out(row, include_segments=include_segments)


async def _build_ready_manifest_from_db(video_id: str) -> dict:
    async with pool.acquire() as conn:
        video_row = await conn.fetchrow(
            """
            SELECT title, original_filename, description, created_at,
                   show_original_filename, show_manifest_address
            FROM videos WHERE id=$1
            """,
            video_id,
        )
        if not video_row:
            raise HTTPException(404, "Video not found")

        variants = await conn.fetch(
            """
            SELECT id, resolution, width, height, video_bitrate, audio_bitrate,
                   segment_duration, total_duration
            FROM video_variants
            WHERE video_id=$1
            ORDER BY height DESC
            """,
            video_id,
        )

        manifest_variants = []
        for variant in variants:
            uploaded_segments = await conn.fetch(
                """
                SELECT segment_index, autonomi_address, duration, byte_size
                FROM video_segments
                WHERE variant_id=$1
                ORDER BY segment_index
                """,
                str(variant["id"]),
            )
            if any(not segment["autonomi_address"] for segment in uploaded_segments):
                raise HTTPException(409, "Video has not finished uploading all segment addresses")
            manifest_variants.append({
                "id": str(variant["id"]),
                "resolution": variant["resolution"],
                "width": variant["width"],
                "height": variant["height"],
                "video_bitrate": variant["video_bitrate"],
                "audio_bitrate": variant["audio_bitrate"],
                "segment_duration": variant["segment_duration"],
                "total_duration": variant["total_duration"],
                "segment_count": len(uploaded_segments),
                "segments": [
                    {
                        "segment_index": segment["segment_index"],
                        "autonomi_address": segment["autonomi_address"],
                        "duration": segment["duration"],
                        "byte_size": segment["byte_size"],
                    }
                    for segment in uploaded_segments
                ],
            })

    show_original_filename = bool(video_row["show_original_filename"])
    return {
        "schema_version": 1,
        "content_type": VIDEO_MANIFEST_CONTENT_TYPE,
        "id": video_id,
        "title": video_row["title"],
        "original_filename": video_row["original_filename"] if show_original_filename else None,
        "description": video_row["description"],
        "status": STATUS_READY,
        "created_at": video_row["created_at"].isoformat(),
        "updated_at": _now_iso(),
        "show_original_filename": show_original_filename,
        "show_manifest_address": bool(video_row["show_manifest_address"]),
        "variants": manifest_variants,
    }


# ── Routes ────────────────────────────────────────────────────────────────────

@app.get("/health")
async def health():
    client = AsyncAntdClient(base_url=ANTD_URL, timeout=10)
    try:
        status = await client.health()
    except AntdError as exc:
        return {
            "ok": False,
            "autonomi": {"ok": False, "error": _format_antd_error(exc)},
        }
    finally:
        await client.close()

    return {
        "ok": status.ok,
        "autonomi": {"ok": status.ok, "network": status.network},
        "payment_mode": ANTD_PAYMENT_MODE,
        "final_quote_approval_ttl_seconds": FINAL_QUOTE_APPROVAL_TTL_SECONDS,
    }


@app.post("/auth/login", response_model=AuthTokenOut)
async def login(request: LoginRequest):
    if not (
        secrets.compare_digest(request.username, ADMIN_USERNAME)
        and secrets.compare_digest(request.password, ADMIN_PASSWORD)
    ):
        raise HTTPException(401, "Invalid username or password")

    token, expires_at = _create_admin_token()
    return AuthTokenOut(
        access_token=token,
        expires_at=expires_at.isoformat(),
        username=ADMIN_USERNAME,
    )


@app.get("/auth/me", response_model=AdminMeOut)
async def auth_me(username: str = Depends(require_admin)):
    return AdminMeOut(username=username)


@app.get("/catalog")
async def get_catalog(username: str = Depends(require_admin)):
    client = AsyncAntdClient(base_url=ANTD_URL, timeout=60)
    try:
        catalog, catalog_address = await _load_catalog(client)
    finally:
        await client.close()
    return {
        "catalog_address": catalog_address,
        "catalog": catalog,
    }


@app.post("/videos/upload/quote", response_model=UploadQuoteOut)
async def quote_video_upload(
    request: UploadQuoteRequest,
    username: str = Depends(require_admin),
):
    """Return an Autonomi price quote for the selected transcoded renditions."""
    return await _build_upload_quote(
        request.duration_seconds,
        request.resolutions,
        _request_source_dimensions(request),
    )


@app.post("/videos/upload", response_model=VideoOut)
async def upload_video(
    username: str = Depends(require_admin),
    file: UploadFile = File(...),
    title: str = Form(...),
    description: str = Form(""),
    resolutions: str = Form("720p"),  # comma-separated, e.g. "360p,720p,1080p,1440p"
    show_original_filename: bool = Form(False),
    show_manifest_address: bool = Form(False),
    content_length: int | None = Header(None),
):
    """Accept a video file and queue it for transcoding + Autonomi upload."""
    selected = [r.strip() for r in resolutions.split(",") if r.strip() in RESOLUTION_PRESETS]
    if not selected:
        raise HTTPException(400, f"No valid resolutions. Choose from: {SUPPORTED_RESOLUTIONS}")

    multipart_overhead_allowance = 2 * 1024 * 1024
    if content_length and content_length > UPLOAD_MAX_FILE_BYTES + multipart_overhead_allowance:
        raise HTTPException(
            413,
            "Upload exceeds max file size "
            f"({_format_bytes(UPLOAD_MAX_FILE_BYTES)})",
        )

    video_id = str(uuid.uuid4())
    job_dir = UPLOAD_TEMP_DIR / video_id
    job_dir.mkdir(parents=True, exist_ok=True)

    safe_filename = _sanitize_upload_filename(file.filename)
    src_path = job_dir / f"original_{safe_filename}"
    tmp_src_path = src_path.with_name(f"{src_path.name}.uploading")

    try:
        await asyncio.wait_for(upload_save_semaphore.acquire(), timeout=0.01)
    except asyncio.TimeoutError as exc:
        shutil.rmtree(job_dir, ignore_errors=True)
        raise HTTPException(429, "Too many uploads are in progress; try again shortly") from exc

    try:
        if content_length:
            _ensure_upload_disk_space(content_length)
        else:
            _ensure_upload_disk_space()

        bytes_written = await _save_upload_stream(file, tmp_src_path)
        duration_seconds, source_dimensions = await _probe_upload_media(tmp_src_path)
        tmp_src_path.replace(src_path)
        log.info(
            "Accepted upload %s filename=%s bytes=%d duration=%.2fs dimensions=%sx%s",
            video_id,
            safe_filename,
            bytes_written,
            duration_seconds,
            source_dimensions[0],
            source_dimensions[1],
        )
    except HTTPException:
        shutil.rmtree(job_dir, ignore_errors=True)
        raise
    except OSError as exc:
        shutil.rmtree(job_dir, ignore_errors=True)
        raise HTTPException(507, f"Could not store upload safely: {exc}") from exc
    finally:
        upload_save_semaphore.release()

    try:
        async with pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                INSERT INTO videos (
                    id, title, original_filename, description, status, job_dir,
                    job_source_path, requested_resolutions,
                    show_original_filename, show_manifest_address, user_id
                )
                VALUES ($1, $2, $3, $4, 'pending', $5, $6, $7::jsonb, $8, $9, $10)
                RETURNING id, title, original_filename, description, status, created_at,
                          show_original_filename, show_manifest_address
                """,
                video_id,
                title,
                safe_filename,
                description or None,
                str(job_dir),
                str(src_path),
                json.dumps(selected),
                show_original_filename,
                show_manifest_address,
                username,
            )
    except Exception:
        shutil.rmtree(job_dir, ignore_errors=True)
        raise

    _schedule_processing_job(video_id, src_path, selected, job_dir)

    return VideoOut(
        id=str(row["id"]),
        title=row["title"],
        original_filename=row["original_filename"],
        description=row["description"],
        status=row["status"],
        created_at=str(row["created_at"]),
        catalog_address=_read_catalog_address(),
        show_original_filename=row["show_original_filename"],
        show_manifest_address=row["show_manifest_address"],
    )


@app.get("/videos", response_model=list[VideoOut])
async def list_videos():
    videos: list[VideoOut] = []
    client = AsyncAntdClient(base_url=ANTD_URL, timeout=60)
    try:
        catalog, catalog_address = await _load_catalog(client)
        for entry in catalog.get("videos", []):
            if entry.get("status", STATUS_READY) == STATUS_READY:
                videos.append(_public_entry_to_video_out(entry, catalog_address))
    finally:
        await client.close()
    return videos


@app.get("/admin/videos", response_model=list[VideoOut])
async def admin_list_videos(username: str = Depends(require_admin)):
    async with pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT id, title, original_filename, description, status, created_at,
                   manifest_address, catalog_address, error_message, final_quote,
                   final_quote_created_at, approval_expires_at,
                   is_public, show_original_filename, show_manifest_address
            FROM videos
            ORDER BY created_at DESC
            """
        )
    return [await _db_video_to_out(row) for row in rows]


@app.get("/videos/{video_id}", response_model=VideoOut)
async def get_video(video_id: str):
    client = AsyncAntdClient(base_url=ANTD_URL, timeout=60)
    try:
        loaded = await _load_video_manifest(client, video_id)
        if loaded:
            manifest, manifest_address = loaded
            return _manifest_to_video_out(manifest, manifest_address, public=True)
    finally:
        await client.close()

    raise HTTPException(404, "Video not found")


@app.get("/admin/videos/{video_id}", response_model=VideoOut)
async def admin_get_video(video_id: str, username: str = Depends(require_admin)):
    return await _get_db_video(video_id, include_segments=True)


@app.patch("/admin/videos/{video_id}/visibility", response_model=VideoOut)
async def update_video_visibility(
    video_id: str,
    request: VideoVisibilityUpdate,
    username: str = Depends(require_admin),
):
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            UPDATE videos
            SET show_original_filename=$1,
                show_manifest_address=$2,
                updated_at=NOW()
            WHERE id=$3
            RETURNING status, is_public
            """,
            request.show_original_filename,
            request.show_manifest_address,
            video_id,
        )
    if not row:
        raise HTTPException(404, "Video not found")

    if row["status"] == STATUS_READY and row["is_public"]:
        client = AsyncAntdClient(
            base_url=ANTD_URL,
            timeout=max(ANTD_UPLOAD_TIMEOUT_SECONDS + 30, 60),
        )
        try:
            manifest = await _build_ready_manifest_from_db(video_id)
            manifest_address, catalog_address = await _publish_video_to_catalog(client, manifest)
        finally:
            await client.close()
        await _set_publication(video_id, True, manifest_address, catalog_address)

    return await _get_db_video(video_id, include_segments=True)


@app.patch("/admin/videos/{video_id}/publication", response_model=VideoOut)
async def update_video_publication(
    video_id: str,
    request: VideoPublicationUpdate,
    username: str = Depends(require_admin),
):
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT status FROM videos WHERE id=$1",
            video_id,
        )
    if not row:
        raise HTTPException(404, "Video not found")

    if request.is_public:
        if row["status"] != STATUS_READY:
            raise HTTPException(409, "Only ready videos can be published")

        client = AsyncAntdClient(
            base_url=ANTD_URL,
            timeout=max(ANTD_UPLOAD_TIMEOUT_SECONDS + 30, 60),
        )
        try:
            manifest = await _build_ready_manifest_from_db(video_id)
            manifest_address, catalog_address = await _publish_video_to_catalog(client, manifest)
        finally:
            await client.close()
        await _set_publication(video_id, True, manifest_address, catalog_address)
    else:
        catalog_address = await _remove_video_from_catalog(video_id)
        await _set_publication(video_id, False, catalog_address=catalog_address)

    return await _get_db_video(video_id, include_segments=True)


@app.get("/videos/{video_id}/status")
async def video_status(video_id: str):
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            SELECT status, manifest_address, catalog_address, error_message,
                   final_quote, final_quote_created_at, approval_expires_at,
                   show_manifest_address
            FROM videos WHERE id=$1
            """,
            video_id,
        )
    if not row:
        client = AsyncAntdClient(base_url=ANTD_URL, timeout=60)
        try:
            loaded = await _load_video_manifest(client, video_id)
        finally:
            await client.close()
        if not loaded:
            raise HTTPException(404, "Video not found")
        manifest, manifest_address = loaded
        show_manifest_address = bool(manifest.get("show_manifest_address"))
        return {
            "video_id": video_id,
            "status": STATUS_READY,
            "manifest_address": manifest_address if show_manifest_address else None,
            "catalog_address": None,
        }
    return {
        "video_id": video_id,
        "status": row["status"],
        "manifest_address": row["manifest_address"] if row["show_manifest_address"] else None,
        "catalog_address": None,
        "error_message": row["error_message"],
    }


@app.post("/admin/videos/{video_id}/approve", response_model=VideoOut)
@app.post("/videos/{video_id}/approve", response_model=VideoOut)
async def approve_video(
    video_id: str,
    username: str = Depends(require_admin),
):
    """Approve the final quote and start the Autonomi upload/publish stage."""
    await _cleanup_expired_approvals()
    expired = False
    expired_job_dir = None
    async with pool.acquire() as conn:
        async with conn.transaction():
            row = await conn.fetchrow(
                """
                SELECT status, approval_expires_at, job_dir
                FROM videos
                WHERE id=$1
                FOR UPDATE
                """,
                video_id,
            )
            if not row:
                raise HTTPException(404, "Video not found")
            if row["status"] != STATUS_AWAITING_APPROVAL:
                raise HTTPException(409, f"Video is {row['status']}, not awaiting approval")
            if row["approval_expires_at"] and row["approval_expires_at"] <= datetime.now(timezone.utc):
                expired = True
                expired_job_dir = row["job_dir"]
                await conn.execute(
                    """
                    UPDATE videos
                    SET status='expired',
                        error_message='Final quote approval window expired; local files were deleted.',
                        updated_at=NOW()
                    WHERE id=$1
                    """,
                    video_id,
                )
            elif not row["job_dir"] or not Path(row["job_dir"]).exists():
                raise HTTPException(410, "Transcoded files are no longer available")
            else:
                await conn.execute(
                    """
                    UPDATE videos
                    SET status='uploading', error_message=NULL, updated_at=NOW()
                    WHERE id=$1
                    """,
                    video_id,
                )

    if expired:
        if expired_job_dir:
            shutil.rmtree(expired_job_dir, ignore_errors=True)
        raise HTTPException(410, "Final quote approval window has expired")

    _schedule_upload_job(video_id)
    return await _get_db_video(video_id, include_segments=True)


@app.delete("/admin/videos/{video_id}")
@app.delete("/videos/{video_id}")
async def delete_video(video_id: str, username: str = Depends(require_admin)):
    catalog_address = await _remove_video_from_catalog(video_id)
    async with pool.acquire() as conn:
        result = await conn.execute("DELETE FROM videos WHERE id=$1", video_id)
    if result == "DELETE 0" and catalog_address is None:
        raise HTTPException(404, "Video not found")
    # Clean up temp files if still present
    job_dir = UPLOAD_TEMP_DIR / video_id
    if job_dir.exists():
        shutil.rmtree(job_dir, ignore_errors=True)
    return {"deleted": video_id, "catalog_address": catalog_address}


# ── Background processing ─────────────────────────────────────────────────────

async def _process_video(
    video_id: str,
    src_path: Path,
    resolutions: list[str],
    job_dir: Path,
    *,
    reset_existing: bool = False,
):
    """Transcode to HLS, produce a final quote, then pause for approval."""
    try:
        if reset_existing:
            async with pool.acquire() as conn:
                await conn.execute("DELETE FROM video_variants WHERE video_id=$1", video_id)
            for res in resolutions:
                shutil.rmtree(job_dir / res, ignore_errors=True)

        await _set_status(video_id, STATUS_PROCESSING)
        async with pool.acquire() as conn:
            video_row = await conn.fetchrow(
                """
                SELECT title, original_filename, description, created_at
                FROM videos WHERE id=$1
                """,
                video_id,
            )
        if not video_row:
            raise RuntimeError(f"Video row {video_id} disappeared before processing")

        total_duration = await _probe_duration(src_path)
        source_dimensions = await _probe_video_dimensions(src_path)

        for res in resolutions:
            preset_width, preset_height, vbitrate, abitrate = RESOLUTION_PRESETS[res]
            width, height = _target_dimensions_for_source(
                preset_width,
                preset_height,
                source_dimensions,
            )
            vbitrate = _target_video_bitrate_kbps(
                vbitrate,
                preset_width,
                preset_height,
                width,
                height,
            )
            seg_dir = job_dir / res
            seg_dir.mkdir(exist_ok=True)

            log.info("Transcoding %s -> %s", video_id, res)
            await _run_ffmpeg(src_path, seg_dir, width, height, vbitrate, abitrate)

            # Collect segments produced by FFmpeg (sorted by index)
            ts_files = sorted(seg_dir.glob("seg_*.ts"), key=lambda p: int(p.stem.split("_")[1]))
            if not ts_files:
                raise RuntimeError(f"FFmpeg produced no segments for {res}")

            # Insert variant record
            async with pool.acquire() as conn:
                variant_row = await conn.fetchrow(
                    """
                    INSERT INTO video_variants
                        (video_id, resolution, width, height, video_bitrate, audio_bitrate,
                         segment_duration, total_duration, segment_count)
                    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                    RETURNING id
                    """,
                    video_id,
                    res,
                    width,
                    height,
                    vbitrate * 1000,
                    abitrate * 1000,
                    float(HLS_SEGMENT_DURATION),
                    total_duration,
                    len(ts_files),
                )
                variant_id = str(variant_row["id"])

            for idx, ts_path in enumerate(ts_files):
                duration = await _probe_duration(ts_path) or float(HLS_SEGMENT_DURATION)
                async with pool.acquire() as conn:
                    await conn.execute(
                        """
                        INSERT INTO video_segments
                            (variant_id, segment_index, duration, byte_size, local_path)
                        VALUES ($1,$2,$3,$4,$5)
                        ON CONFLICT (variant_id, segment_index) DO UPDATE
                          SET duration=EXCLUDED.duration,
                              byte_size=EXCLUDED.byte_size,
                              local_path=EXCLUDED.local_path
                        """,
                        variant_id, idx, duration, ts_path.stat().st_size, str(ts_path),
                    )

        final_quote = await _build_final_upload_quote(video_id)
        expires_at = datetime.now(timezone.utc) + timedelta(
            seconds=FINAL_QUOTE_APPROVAL_TTL_SECONDS
        )
        final_quote["approval_expires_at"] = expires_at.isoformat()
        final_quote["quote_created_at"] = _now_iso()
        await _set_awaiting_approval(video_id, final_quote, expires_at)
        log.info(
            "Video %s is awaiting approval final_cost=%s expires_at=%s",
            video_id,
            final_quote["storage_cost_atto"],
            expires_at.isoformat(),
        )

    except Exception as exc:
        log.exception("Processing failed for %s: %s", video_id, exc)
        await _set_status(video_id, STATUS_ERROR, str(exc))
        shutil.rmtree(job_dir, ignore_errors=True)


async def _upload_approved_video(video_id: str):
    """Upload approved segment files, publish manifests, and clean local files."""
    job_dir: Path | None = None
    try:
        async with pool.acquire() as conn:
            video_row = await conn.fetchrow(
                """
                SELECT title, original_filename, description, created_at, job_dir,
                       show_original_filename, show_manifest_address
                FROM videos WHERE id=$1
                """,
                video_id,
            )
        if not video_row:
            raise RuntimeError(f"Video row {video_id} disappeared before upload")

        job_dir = Path(video_row["job_dir"]) if video_row["job_dir"] else None
        manifest = {
            "schema_version": 1,
            "content_type": VIDEO_MANIFEST_CONTENT_TYPE,
            "id": video_id,
            "title": video_row["title"],
            "original_filename": (
                video_row["original_filename"] if video_row["show_original_filename"] else None
            ),
            "description": video_row["description"],
            "status": STATUS_READY,
            "created_at": video_row["created_at"].isoformat(),
            "updated_at": _now_iso(),
            "show_original_filename": bool(video_row["show_original_filename"]),
            "show_manifest_address": bool(video_row["show_manifest_address"]),
            "variants": [],
        }

        client = AsyncAntdClient(
            base_url=ANTD_URL,
            timeout=max(ANTD_UPLOAD_TIMEOUT_SECONDS + 30, 60),
        )
        try:
            async with pool.acquire() as conn:
                variants = await conn.fetch(
                    """
                    SELECT id, resolution, width, height, video_bitrate, audio_bitrate,
                           segment_duration, total_duration
                    FROM video_variants
                    WHERE video_id=$1
                    ORDER BY height DESC
                    """,
                    video_id,
                )

            for variant in variants:
                variant_id = str(variant["id"])
                async with pool.acquire() as conn:
                    segment_rows = await conn.fetch(
                        """
                        SELECT segment_index, local_path, duration, byte_size,
                               autonomi_address
                        FROM video_segments
                        WHERE variant_id=$1
                        ORDER BY segment_index
                        """,
                        variant_id,
                    )

                if not segment_rows:
                    raise RuntimeError(f"No segments found for {variant['resolution']}")

                log.info(
                    "Uploading %d approved segments for %s/%s with payment_mode=%s",
                    len(segment_rows),
                    video_id,
                    variant["resolution"],
                    ANTD_PAYMENT_MODE,
                )
                for segment in segment_rows:
                    if segment["autonomi_address"]:
                        log.info(
                            "  seg %03d already uploaded -> %s",
                            segment["segment_index"],
                            segment["autonomi_address"],
                        )
                        continue

                    ts_path = Path(segment["local_path"] or "")
                    if not ts_path.exists():
                        raise RuntimeError(f"Transcoded segment is missing from disk: {ts_path}")
                    data = ts_path.read_bytes()
                    result = await _put_public_verified(
                        client,
                        data,
                        label=f"{video_id}/{variant['resolution']}/segment-{segment['segment_index']:05d}",
                    )
                    async with pool.acquire() as conn:
                        await conn.execute(
                            """
                            UPDATE video_segments
                            SET autonomi_address=$1,
                                autonomi_cost_atto=$2,
                                autonomi_payment_mode=$3,
                                byte_size=$4
                            WHERE variant_id=$5 AND segment_index=$6
                            """,
                            result.address,
                            result.cost,
                            ANTD_PAYMENT_MODE,
                            len(data),
                            variant_id,
                            segment["segment_index"],
                        )
                    log.info(
                        "  seg %03d -> %s (cost=%s)",
                        segment["segment_index"],
                        result.address,
                        result.cost,
                    )

                async with pool.acquire() as conn:
                    uploaded_segments = await conn.fetch(
                        """
                        SELECT segment_index, autonomi_address, duration, byte_size
                        FROM video_segments
                        WHERE variant_id=$1
                        ORDER BY segment_index
                        """,
                        variant_id,
                    )

                manifest["variants"].append({
                    "id": variant_id,
                    "resolution": variant["resolution"],
                    "width": variant["width"],
                    "height": variant["height"],
                    "video_bitrate": variant["video_bitrate"],
                    "audio_bitrate": variant["audio_bitrate"],
                    "segment_duration": variant["segment_duration"],
                    "total_duration": variant["total_duration"],
                    "segment_count": len(uploaded_segments),
                    "segments": [
                        {
                            "segment_index": segment["segment_index"],
                            "autonomi_address": segment["autonomi_address"],
                            "duration": segment["duration"],
                            "byte_size": segment["byte_size"],
                        }
                        for segment in uploaded_segments
                    ],
                })

            manifest["updated_at"] = _now_iso()
            manifest_address = await _store_json_public(client, manifest)
            catalog_address = _read_catalog_address()
        finally:
            await client.close()

        await _set_ready(video_id, manifest_address, catalog_address)
        if job_dir and job_dir.exists():
            shutil.rmtree(job_dir, ignore_errors=True)
        log.info(
            "Video %s is ready manifest=%s catalog=%s public=false",
            video_id,
            manifest_address,
            catalog_address,
        )
    except Exception as exc:
        log.exception("Approved upload failed for %s: %s", video_id, exc)
        await _set_status(video_id, STATUS_ERROR, str(exc))
        if job_dir and job_dir.exists():
            shutil.rmtree(job_dir, ignore_errors=True)


async def _run_ffmpeg(
    src: Path, seg_dir: Path, width: int, height: int, vbitrate: int, abitrate: int
):
    """Run FFmpeg to produce HLS .ts segments."""
    segment_pattern = str(seg_dir / "seg_%05d.ts")
    segment_time = f"{HLS_SEGMENT_DURATION:g}"
    cmd = [
        "ffmpeg",
        "-hide_banner",
        "-nostats",
        "-loglevel", "warning",
        "-y",
        "-filter_threads", str(FFMPEG_FILTER_THREADS),
        "-i", str(src),
        "-map", "0:v:0",
        "-map", "0:a?",
        "-sn",
        "-c:v", "libx264",
        "-threads", str(FFMPEG_THREADS),
        "-preset", "veryfast",
        "-profile:v", "high",
        "-pix_fmt", "yuv420p",
        "-vf", f"scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2",
        "-b:v", f"{vbitrate}k",
        "-maxrate", f"{int(vbitrate * 1.5)}k",
        "-bufsize", f"{vbitrate * 2}k",
        "-force_key_frames", f"expr:gte(t,n_forced*{segment_time})",
        "-sc_threshold", "0",
        "-c:a", "aac",
        "-b:a", f"{abitrate}k",
        "-ar", "44100",
        "-f", "segment",
        "-segment_time", segment_time,
        "-segment_time_delta", "0.05",
        "-segment_format", "mpegts",
        "-reset_timestamps", "1",
        segment_pattern,
    ]
    proc = await asyncio.create_subprocess_exec(
        *cmd,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    try:
        _, stderr = await proc.communicate()
    except asyncio.CancelledError:
        if proc.returncode is None:
            proc.kill()
        await proc.wait()
        raise
    if proc.returncode != 0:
        detail = stderr.decode(errors="replace").strip()[-2000:]
        if proc.returncode == -9:
            detail = (
                "FFmpeg was killed by signal 9, which usually means the "
                "container ran out of memory while transcoding. "
                f"FFMPEG_THREADS={FFMPEG_THREADS}, "
                f"FFMPEG_FILTER_THREADS={FFMPEG_FILTER_THREADS}. {detail}"
            )
        raise RuntimeError(f"FFmpeg failed with exit code {proc.returncode}: {detail}")


async def _probe_duration(src: Path) -> float | None:
    """Use ffprobe to get video duration in seconds."""
    cmd = [
        "ffprobe", "-v", "quiet",
        "-show_entries", "format=duration",
        "-of", "default=noprint_wrappers=1:nokey=1",
        str(src),
    ]
    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL
    )
    stdout, _ = await proc.communicate()
    try:
        return float(stdout.decode().strip())
    except ValueError:
        return None


def _stream_rotation_degrees(stream: dict) -> int:
    tags = stream.get("tags") or {}
    rotation = tags.get("rotate")
    if rotation is None:
        for side_data in stream.get("side_data_list") or []:
            if "rotation" in side_data:
                rotation = side_data["rotation"]
                break

    try:
        return int(float(str(rotation))) % 360
    except (TypeError, ValueError):
        return 0


def _parse_probe_duration(data: dict, stream: dict) -> float | None:
    for source in (stream, data.get("format") or {}):
        try:
            duration = float(source.get("duration"))
        except (TypeError, ValueError):
            continue
        if math.isfinite(duration) and duration > 0:
            return duration
    return None


async def _probe_upload_media(src: Path) -> UploadMediaMetadata:
    """Validate that an uploaded source has a real video stream we can process."""
    cmd = [
        "ffprobe", "-v", "error",
        "-show_streams",
        "-show_format",
        "-of", "json",
        str(src),
    ]
    try:
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
    except FileNotFoundError as exc:
        raise HTTPException(500, "ffprobe is required to validate uploaded media") from exc
    try:
        stdout, stderr = await asyncio.wait_for(
            proc.communicate(),
            timeout=UPLOAD_FFPROBE_TIMEOUT_SECONDS,
        )
    except asyncio.TimeoutError as exc:
        if proc.returncode is None:
            proc.kill()
        await proc.communicate()
        raise HTTPException(400, "Could not validate uploaded media before timeout") from exc

    if proc.returncode != 0:
        detail = stderr.decode(errors="replace").strip()[-500:]
        message = "Uploaded file is not a readable video"
        if detail:
            message = f"{message}: {detail}"
        raise HTTPException(400, message)

    try:
        data = json.loads(stdout.decode())
    except json.JSONDecodeError as exc:
        raise HTTPException(400, "Uploaded file probe returned invalid metadata") from exc

    stream = next(
        (
            candidate
            for candidate in data.get("streams") or []
            if candidate.get("codec_type") == "video"
        ),
        None,
    )
    if not stream:
        raise HTTPException(400, "Uploaded file does not contain a video stream")

    try:
        width = int(stream["width"])
        height = int(stream["height"])
    except (KeyError, TypeError, ValueError) as exc:
        raise HTTPException(400, "Uploaded video stream has no usable dimensions") from exc

    if width <= 0 or height <= 0:
        raise HTTPException(400, "Uploaded video stream has invalid dimensions")

    if _stream_rotation_degrees(stream) in {90, 270}:
        dimensions = (height, width)
    else:
        dimensions = (width, height)

    duration = _parse_probe_duration(data, stream)
    if duration is None:
        raise HTTPException(400, "Uploaded video has no usable duration")

    _enforce_upload_media_limits(duration, dimensions)
    return duration, dimensions


async def _probe_video_dimensions(src: Path) -> VideoDimensions | None:
    """Use ffprobe to get the source's display dimensions."""
    cmd = [
        "ffprobe", "-v", "quiet",
        "-select_streams", "v:0",
        "-show_streams",
        "-of", "json",
        str(src),
    ]
    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL
    )
    stdout, _ = await proc.communicate()

    try:
        data = json.loads(stdout.decode())
        stream = (data.get("streams") or [{}])[0]
        width = int(stream["width"])
        height = int(stream["height"])
    except (ValueError, KeyError, TypeError, json.JSONDecodeError):
        return None

    if _stream_rotation_degrees(stream) in {90, 270}:
        return height, width
    return width, height


async def _set_status(video_id: str, status: str, error_message: str | None = None):
    async with pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE videos
            SET status=$1, error_message=$2, updated_at=NOW()
            WHERE id=$3
            """,
            status, error_message, video_id,
        )


async def _set_awaiting_approval(video_id: str, final_quote: dict, expires_at: datetime):
    async with pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE videos
            SET status='awaiting_approval',
                final_quote=$1::jsonb,
                final_quote_created_at=NOW(),
                approval_expires_at=$2,
                error_message=NULL,
                updated_at=NOW()
            WHERE id=$3
            """,
            json.dumps(final_quote),
            expires_at,
            video_id,
        )


async def _set_ready(video_id: str, manifest_address: str, catalog_address: str | None):
    async with pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE videos
            SET status='ready',
                manifest_address=$1,
                catalog_address=$2,
                is_public=FALSE,
                error_message=NULL,
                job_dir=NULL,
                job_source_path=NULL,
                approval_expires_at=NULL,
                updated_at=NOW()
            WHERE id=$3
            """,
            manifest_address, catalog_address, video_id,
        )


async def _set_publication(
    video_id: str,
    is_public: bool,
    manifest_address: str | None = None,
    catalog_address: str | None = None,
):
    async with pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE videos
            SET is_public=$1,
                manifest_address=COALESCE($2, manifest_address),
                catalog_address=COALESCE($3, catalog_address),
                updated_at=NOW()
            WHERE id=$4
            """,
            is_public, manifest_address, catalog_address, video_id,
        )
