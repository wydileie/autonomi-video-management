"""
Python Admin Service — video ingestion, FFmpeg transcoding, Autonomi upload.

Flow:
  1. Client POSTs a video file + desired resolutions.
  2. We save it to /tmp, create a DB record, and kick off background processing.
  3. Background task runs FFmpeg to produce HLS .ts segments per resolution.
  4. Each segment is uploaded to the Autonomi network via the antd daemon.
  5. Segment addresses are published in a video manifest on Autonomi.
  6. The network catalog is updated with that manifest address.
  7. The Rust streaming service reads the catalog/manifest from Autonomi.
"""

import asyncio
import json
import logging
import os
import shutil
import uuid
from contextlib import asynccontextmanager
from datetime import datetime, timezone
from pathlib import Path

import asyncpg
from antd import AsyncAntdClient, AntdError
from fastapi import BackgroundTasks, FastAPI, File, Form, HTTPException, UploadFile
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

# ── Config ────────────────────────────────────────────────────────────────────

DB_DSN = (
    f"postgresql://{os.environ['ADMIN_DB_USER']}:{os.environ['ADMIN_DB_PASS']}"
    f"@{os.environ['ADMIN_DB_HOST']}:{os.environ.get('ADMIN_DB_PORT', '5432')}"
    f"/{os.environ['ADMIN_DB_NAME']}"
)
ANTD_URL = os.environ.get("ANTD_URL", "http://localhost:8082")
ANTD_PAYMENT_MODE = os.environ.get("ANTD_PAYMENT_MODE", "auto").strip().lower()
ANTD_APPROVE_ON_STARTUP = os.environ.get("ANTD_APPROVE_ON_STARTUP", "true").lower() not in {
    "0",
    "false",
    "no",
}
UPLOAD_TEMP_DIR = Path(os.environ.get("UPLOAD_TEMP_DIR", "/tmp/video_uploads"))
UPLOAD_TEMP_DIR.mkdir(parents=True, exist_ok=True)
CATALOG_STATE_PATH = Path(os.environ.get("CATALOG_STATE_PATH", "/tmp/video_catalog/catalog.json"))
CATALOG_BOOTSTRAP_ADDRESS = os.environ.get("CATALOG_ADDRESS", "").strip()
CATALOG_CONTENT_TYPE = "application/vnd.autonomi.video.catalog+json;v=1"
VIDEO_MANIFEST_CONTENT_TYPE = "application/vnd.autonomi.video.manifest+json;v=1"

# Resolution presets: name → (width, height, video_kbps, audio_kbps)
RESOLUTION_PRESETS: dict[str, tuple[int, int, int, int]] = {
    "360p":  (640,  360,   500, 96),
    "480p":  (854,  480,  1000, 128),
    "720p":  (1280, 720,  2500, 128),
    "1080p": (1920, 1080, 5000, 192),
}

HLS_SEGMENT_DURATION = 10  # seconds per .ts chunk
VALID_PAYMENT_MODES = {"auto", "merkle", "single"}

if ANTD_PAYMENT_MODE not in VALID_PAYMENT_MODES:
    raise RuntimeError(
        f"Invalid ANTD_PAYMENT_MODE={ANTD_PAYMENT_MODE!r}; "
        f"choose one of {sorted(VALID_PAYMENT_MODES)}"
    )

logging.basicConfig(level=logging.INFO)
log = logging.getLogger(__name__)
catalog_lock = asyncio.Lock()

# ── Database pool ─────────────────────────────────────────────────────────────

pool: asyncpg.Pool | None = None


@asynccontextmanager
async def lifespan(app: FastAPI):
    global pool
    pool = await asyncpg.create_pool(DB_DSN, min_size=2, max_size=10)
    await _ensure_schema()
    _ensure_catalog_state_dir()
    await _ensure_autonomi_ready()
    yield
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
                created_at TIMESTAMPTZ DEFAULT NOW(),
                updated_at TIMESTAMPTZ DEFAULT NOW(),
                user_id TEXT
            );

            ALTER TABLE videos
                ADD COLUMN IF NOT EXISTS manifest_address TEXT,
                ADD COLUMN IF NOT EXISTS catalog_address TEXT,
                ADD COLUMN IF NOT EXISTS error_message TEXT;

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
                autonomi_address TEXT NOT NULL,
                autonomi_cost_atto TEXT,
                autonomi_payment_mode TEXT,
                duration FLOAT NOT NULL DEFAULT 10.0,
                byte_size BIGINT,
                created_at TIMESTAMPTZ DEFAULT NOW(),
                UNIQUE (variant_id, segment_index)
            );

            ALTER TABLE video_segments
                ADD COLUMN IF NOT EXISTS autonomi_cost_atto TEXT,
                ADD COLUMN IF NOT EXISTS autonomi_payment_mode TEXT;

            CREATE INDEX IF NOT EXISTS idx_videos_status ON videos(status);
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
        raise RuntimeError(f"Autonomi daemon is not ready at {ANTD_URL}: {exc}") from exc
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
    result = await client.data_put_public(data, payment_mode=ANTD_PAYMENT_MODE)
    return result.address


def _video_catalog_entry(manifest: dict, manifest_address: str) -> dict:
    return {
        "id": manifest["id"],
        "title": manifest["title"],
        "original_filename": manifest["original_filename"],
        "description": manifest.get("description"),
        "status": "ready",
        "created_at": manifest["created_at"],
        "updated_at": manifest["updated_at"],
        "manifest_address": manifest_address,
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


async def _remove_video_from_catalog(video_id: str) -> str | None:
    async with catalog_lock:
        client = AsyncAntdClient(base_url=ANTD_URL, timeout=300)
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


def _manifest_to_video_out(manifest: dict, manifest_address: str | None = None) -> "VideoOut":
    return VideoOut(
        id=manifest["id"],
        title=manifest["title"],
        original_filename=manifest["original_filename"],
        description=manifest.get("description"),
        status=manifest.get("status", "ready"),
        created_at=manifest["created_at"],
        manifest_address=manifest_address or manifest.get("manifest_address"),
        catalog_address=_read_catalog_address(),
        variants=[
            VariantOut(
                id=f"{manifest['id']}:{variant['resolution']}",
                resolution=variant["resolution"],
                width=variant["width"],
                height=variant["height"],
                total_duration=variant.get("total_duration"),
                segment_count=variant.get("segment_count"),
                segments=[
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

app = FastAPI(title="Autonomi Video Admin", lifespan=lifespan)

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["*"],
    allow_headers=["*"],
)


# ── Pydantic models ───────────────────────────────────────────────────────────

class SegmentOut(BaseModel):
    segment_index: int
    autonomi_address: str
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
    original_filename: str
    description: str | None
    status: str
    created_at: str
    manifest_address: str | None = None
    catalog_address: str | None = None
    variants: list[VariantOut] = []


# ── Routes ────────────────────────────────────────────────────────────────────

@app.get("/health")
async def health():
    client = AsyncAntdClient(base_url=ANTD_URL, timeout=10)
    try:
        status = await client.health()
    except AntdError as exc:
        return {"ok": False, "autonomi": {"ok": False, "error": str(exc)}}
    finally:
        await client.close()

    return {
        "ok": status.ok,
        "autonomi": {"ok": status.ok, "network": status.network},
        "payment_mode": ANTD_PAYMENT_MODE,
        "catalog_address": _read_catalog_address(),
    }


@app.get("/catalog")
async def get_catalog():
    client = AsyncAntdClient(base_url=ANTD_URL, timeout=60)
    try:
        catalog, catalog_address = await _load_catalog(client)
    finally:
        await client.close()
    return {
        "catalog_address": catalog_address,
        "catalog": catalog,
    }


@app.post("/videos/upload", response_model=VideoOut)
async def upload_video(
    background_tasks: BackgroundTasks,
    file: UploadFile = File(...),
    title: str = Form(...),
    description: str = Form(""),
    resolutions: str = Form("720p"),  # comma-separated, e.g. "480p,720p,1080p"
):
    """Accept a video file and queue it for transcoding + Autonomi upload."""
    selected = [r.strip() for r in resolutions.split(",") if r.strip() in RESOLUTION_PRESETS]
    if not selected:
        raise HTTPException(400, f"No valid resolutions. Choose from: {list(RESOLUTION_PRESETS)}")

    video_id = str(uuid.uuid4())
    job_dir = UPLOAD_TEMP_DIR / video_id
    job_dir.mkdir(parents=True, exist_ok=True)

    # Save raw upload
    src_path = job_dir / f"original_{file.filename}"
    with open(src_path, "wb") as f_out:
        while chunk := await file.read(1024 * 1024):
            f_out.write(chunk)

    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            INSERT INTO videos (id, title, original_filename, description, status)
            VALUES ($1, $2, $3, $4, 'pending')
            RETURNING id, title, original_filename, description, status, created_at
            """,
            video_id, title, file.filename, description or None,
        )

    background_tasks.add_task(
        _process_video, video_id, src_path, selected, job_dir
    )

    return VideoOut(
        id=str(row["id"]),
        title=row["title"],
        original_filename=row["original_filename"],
        description=row["description"],
        status=row["status"],
        created_at=str(row["created_at"]),
        catalog_address=_read_catalog_address(),
    )


@app.get("/videos", response_model=list[VideoOut])
async def list_videos():
    videos: list[VideoOut] = []
    client = AsyncAntdClient(base_url=ANTD_URL, timeout=60)
    try:
        catalog, catalog_address = await _load_catalog(client)
        for entry in catalog.get("videos", []):
            videos.append(VideoOut(
                id=entry["id"],
                title=entry["title"],
                original_filename=entry["original_filename"],
                description=entry.get("description"),
                status=entry.get("status", "ready"),
                created_at=entry["created_at"],
                manifest_address=entry.get("manifest_address"),
                catalog_address=catalog_address,
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
            ))
    finally:
        await client.close()

    async with pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT id, title, original_filename, description, status, created_at,
                   manifest_address, catalog_address
            FROM videos
            WHERE status IN ('pending', 'processing', 'error')
            ORDER BY created_at DESC
            """
        )
    network_ids = {video.id for video in videos}
    videos.extend(
        VideoOut(
            id=str(r["id"]),
            title=r["title"],
            original_filename=r["original_filename"],
            description=r["description"],
            status=r["status"],
            created_at=str(r["created_at"]),
            manifest_address=r["manifest_address"],
            catalog_address=r["catalog_address"] or _read_catalog_address(),
        )
        for r in rows
        if str(r["id"]) not in network_ids
    )
    return videos


@app.get("/videos/{video_id}", response_model=VideoOut)
async def get_video(video_id: str):
    client = AsyncAntdClient(base_url=ANTD_URL, timeout=60)
    try:
        loaded = await _load_video_manifest(client, video_id)
        if loaded:
            manifest, manifest_address = loaded
            return _manifest_to_video_out(manifest, manifest_address)
    finally:
        await client.close()

    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            SELECT id, title, original_filename, description, status, created_at,
                   manifest_address, catalog_address
            FROM videos WHERE id=$1
            """,
            video_id,
        )
        if not row:
            raise HTTPException(404, "Video not found")

        variants_rows = await conn.fetch(
            """
            SELECT id, resolution, width, height, total_duration, segment_count
            FROM video_variants WHERE video_id=$1 ORDER BY height
            """,
            video_id,
        )
        variants = []
        for v in variants_rows:
            seg_rows = await conn.fetch(
                """
                SELECT segment_index, autonomi_address, duration
                FROM video_segments WHERE variant_id=$1 ORDER BY segment_index
                """,
                str(v["id"]),
            )
            variants.append(VariantOut(
                id=str(v["id"]),
                resolution=v["resolution"],
                width=v["width"],
                height=v["height"],
                total_duration=v["total_duration"],
                segment_count=v["segment_count"],
                segments=[
                    SegmentOut(
                        segment_index=s["segment_index"],
                        autonomi_address=s["autonomi_address"],
                        duration=s["duration"],
                    )
                    for s in seg_rows
                ],
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
        variants=variants,
    )


@app.get("/videos/{video_id}/status")
async def video_status(video_id: str):
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT status, manifest_address, catalog_address FROM videos WHERE id=$1", video_id
        )
    if not row:
        client = AsyncAntdClient(base_url=ANTD_URL, timeout=60)
        try:
            loaded = await _load_video_manifest(client, video_id)
        finally:
            await client.close()
        if not loaded:
            raise HTTPException(404, "Video not found")
        _, manifest_address = loaded
        return {
            "video_id": video_id,
            "status": "ready",
            "manifest_address": manifest_address,
            "catalog_address": _read_catalog_address(),
        }
    return {
        "video_id": video_id,
        "status": row["status"],
        "manifest_address": row["manifest_address"],
        "catalog_address": row["catalog_address"] or _read_catalog_address(),
    }


@app.delete("/videos/{video_id}")
async def delete_video(video_id: str):
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
):
    """Transcode to HLS at each resolution, upload segments to Autonomi."""
    try:
        await _set_status(video_id, "processing")
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

        manifest = {
            "schema_version": 1,
            "content_type": VIDEO_MANIFEST_CONTENT_TYPE,
            "id": video_id,
            "title": video_row["title"],
            "original_filename": video_row["original_filename"],
            "description": video_row["description"],
            "status": "ready",
            "created_at": video_row["created_at"].isoformat(),
            "updated_at": _now_iso(),
            "variants": [],
        }

        client = AsyncAntdClient(base_url=ANTD_URL, timeout=300)
        try:
            for res in resolutions:
                width, height, vbitrate, abitrate = RESOLUTION_PRESETS[res]
                seg_dir = job_dir / res
                seg_dir.mkdir(exist_ok=True)

                log.info("Transcoding %s -> %s", video_id, res)
                await _run_ffmpeg(src_path, seg_dir, width, height, vbitrate, abitrate)

                # Collect segments produced by FFmpeg (sorted by index)
                ts_files = sorted(seg_dir.glob("seg_*.ts"), key=lambda p: int(p.stem.split("_")[1]))
                if not ts_files:
                    raise RuntimeError(f"FFmpeg produced no segments for {res}")

                total_duration = await _probe_duration(src_path)

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
                        video_id, res, width, height, vbitrate * 1000, abitrate * 1000,
                        float(HLS_SEGMENT_DURATION), total_duration, len(ts_files),
                    )
                    variant_id = str(variant_row["id"])

                # Upload each segment as public immutable data for direct HLS proxy reads.
                log.info(
                    "Uploading %d segments for %s/%s with payment_mode=%s",
                    len(ts_files),
                    video_id,
                    res,
                    ANTD_PAYMENT_MODE,
                )
                for idx, ts_path in enumerate(ts_files):
                    data = ts_path.read_bytes()
                    duration = await _probe_duration(ts_path) or float(HLS_SEGMENT_DURATION)
                    try:
                        result = await client.data_put_public(
                            data,
                            payment_mode=ANTD_PAYMENT_MODE,
                        )
                        address = result.address
                    except AntdError as e:
                        raise RuntimeError(f"Autonomi upload failed for segment {idx}: {e}") from e

                    async with pool.acquire() as conn:
                        await conn.execute(
                            """
                            INSERT INTO video_segments
                                (variant_id, segment_index, autonomi_address,
                                 autonomi_cost_atto, autonomi_payment_mode, duration, byte_size)
                            VALUES ($1,$2,$3,$4,$5,$6,$7)
                            ON CONFLICT (variant_id, segment_index) DO UPDATE
                              SET autonomi_address=EXCLUDED.autonomi_address,
                                  autonomi_cost_atto=EXCLUDED.autonomi_cost_atto,
                                  autonomi_payment_mode=EXCLUDED.autonomi_payment_mode,
                                  duration=EXCLUDED.duration,
                                  byte_size=EXCLUDED.byte_size
                            """,
                            variant_id,
                            idx,
                            address,
                            result.cost,
                            ANTD_PAYMENT_MODE,
                            duration,
                            len(data),
                        )
                    log.info("  seg %03d -> %s (cost=%s)", idx, address, result.cost)

                async with pool.acquire() as conn:
                    seg_rows = await conn.fetch(
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
                    "resolution": res,
                    "width": width,
                    "height": height,
                    "video_bitrate": vbitrate * 1000,
                    "audio_bitrate": abitrate * 1000,
                    "segment_duration": float(HLS_SEGMENT_DURATION),
                    "total_duration": total_duration,
                    "segment_count": len(seg_rows),
                    "segments": [
                        {
                            "segment_index": s["segment_index"],
                            "autonomi_address": s["autonomi_address"],
                            "duration": s["duration"],
                            "byte_size": s["byte_size"],
                        }
                        for s in seg_rows
                    ],
                })

            manifest["updated_at"] = _now_iso()
            manifest_address, catalog_address = await _publish_video_to_catalog(client, manifest)
        finally:
            await client.close()

        await _set_ready(video_id, manifest_address, catalog_address)
        log.info(
            "Video %s is ready manifest=%s catalog=%s",
            video_id,
            manifest_address,
            catalog_address,
        )

    except Exception as exc:
        log.exception("Processing failed for %s: %s", video_id, exc)
        await _set_status(video_id, "error", str(exc))
    finally:
        # Clean up temp files
        shutil.rmtree(job_dir, ignore_errors=True)


async def _run_ffmpeg(
    src: Path, seg_dir: Path, width: int, height: int, vbitrate: int, abitrate: int
):
    """Run FFmpeg to produce HLS .ts segments."""
    segment_pattern = str(seg_dir / "seg_%05d.ts")
    cmd = [
        "ffmpeg", "-y",
        "-i", str(src),
        "-c:v", "libx264",
        "-profile:v", "main",
        "-level", "3.1",
        "-vf", f"scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2",
        "-b:v", f"{vbitrate}k",
        "-maxrate", f"{int(vbitrate * 1.5)}k",
        "-bufsize", f"{vbitrate * 2}k",
        "-c:a", "aac",
        "-b:a", f"{abitrate}k",
        "-ar", "44100",
        "-f", "segment",
        "-segment_time", str(HLS_SEGMENT_DURATION),
        "-segment_format", "mpegts",
        "-reset_timestamps", "1",
        segment_pattern,
    ]
    proc = await asyncio.create_subprocess_exec(
        *cmd,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    _, stderr = await proc.communicate()
    if proc.returncode != 0:
        raise RuntimeError(f"FFmpeg failed: {stderr.decode()[-2000:]}")


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


async def _set_ready(video_id: str, manifest_address: str, catalog_address: str):
    async with pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE videos
            SET status='ready',
                manifest_address=$1,
                catalog_address=$2,
                error_message=NULL,
                updated_at=NOW()
            WHERE id=$3
            """,
            manifest_address, catalog_address, video_id,
        )
