import asyncio
import json
import os
import shutil
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

os.environ.setdefault("ADMIN_DB_USER", "test")
os.environ.setdefault("ADMIN_DB_PASS", "test")
os.environ.setdefault("ADMIN_DB_HOST", "localhost")
os.environ.setdefault("ADMIN_DB_NAME", "test")
os.environ.setdefault("ADMIN_USERNAME", "admin")
os.environ.setdefault("ADMIN_PASSWORD", "admin")
os.environ.setdefault("ADMIN_AUTH_SECRET", "test-secret")
os.environ.setdefault("CORS_ALLOWED_ORIGINS", "http://localhost")

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from fastapi import HTTPException
from fastapi.testclient import TestClient

from src import admin_service as admin


VIDEO_ID = "11111111-1111-1111-1111-111111111111"
VARIANT_ID = "22222222-2222-2222-2222-222222222222"


class FakeTransaction:
    async def __aenter__(self):
        return self

    async def __aexit__(self, exc_type, exc, tb):
        return False


class FakeAcquire:
    def __init__(self, conn):
        self.conn = conn

    async def __aenter__(self):
        return self.conn

    async def __aexit__(self, exc_type, exc, tb):
        return False


class FakePool:
    def __init__(self, store):
        self.conn = FakeConnection(store)

    def acquire(self):
        return FakeAcquire(self.conn)


class FakeConnection:
    def __init__(self, store):
        self.store = store

    def transaction(self):
        return FakeTransaction()

    async def fetchrow(self, query, *args):
        query = " ".join(query.split())
        if "FROM videos" in query and "WHERE id=$1" in query:
            return self.store["videos"].get(str(args[0]))
        return None

    async def fetch(self, query, *args):
        query = " ".join(query.split())

        if query.startswith("UPDATE videos SET status='expired'"):
            return []

        if "FROM video_variants" in query and "WHERE video_id=$1" in query:
            return [
                row
                for row in self.store["variants"]
                if str(row["video_id"]) == str(args[0])
            ]

        if "FROM video_segments" in query and "WHERE variant_id=$1" in query:
            return [
                row
                for row in self.store["segments"]
                if str(row["variant_id"]) == str(args[0])
            ]

        if "FROM videos" in query:
            return list(self.store["videos"].values())

        return []

    async def execute(self, query, *args):
        query = " ".join(query.split())

        if "SET status='uploading'" in query:
            row = self.store["videos"].get(str(args[0]))
            if row:
                row["status"] = "uploading"
                row["error_message"] = None
            return "UPDATE 1"

        if "SET status='expired'" in query:
            row = self.store["videos"].get(str(args[0]))
            if row:
                row["status"] = "expired"
                row["error_message"] = (
                    "Final quote approval window expired; local files were deleted."
                )
            return "UPDATE 1"

        if "SET status=$1" in query:
            status, error_message, video_id = args
            row = self.store["videos"].get(str(video_id))
            if row:
                row["status"] = status
                row["error_message"] = error_message
            return "UPDATE 1"

        if "SET autonomi_address=$1" in query:
            address, cost, payment_mode, byte_size, variant_id, segment_index = args
            for row in self.store["segments"]:
                if (
                    str(row["variant_id"]) == str(variant_id)
                    and row["segment_index"] == segment_index
                ):
                    row["autonomi_address"] = address
                    row["autonomi_cost_atto"] = cost
                    row["autonomi_payment_mode"] = payment_mode
                    row["byte_size"] = byte_size
                    return "UPDATE 1"
            return "UPDATE 0"

        if "SET status='ready'" in query:
            manifest_address, catalog_address, video_id = args
            row = self.store["videos"].get(str(video_id))
            if row:
                row["status"] = "ready"
                row["manifest_address"] = manifest_address
                row["catalog_address"] = catalog_address
                row["is_public"] = False
                row["error_message"] = None
                row["job_dir"] = None
                row["job_source_path"] = None
                row["approval_expires_at"] = None
            return "UPDATE 1"

        return "UPDATE 0"


class FakeAntdClient:
    storage = {}
    puts = 0

    def __init__(self, base_url=None, timeout=None):
        self.base_url = base_url
        self.timeout = timeout

    async def close(self):
        return None

    async def data_put_public(self, data, payment_mode="auto"):
        self.__class__.puts += 1
        address = f"addr-{self.__class__.puts}"
        self.__class__.storage[address] = data
        return SimpleNamespace(
            address=address,
            chunks_stored=1,
            payment_mode_used=payment_mode,
            cost=str(len(data)),
        )

    async def data_get_public(self, address):
        return self.__class__.storage[address]


class MismatchingAntdClient(FakeAntdClient):
    async def data_get_public(self, address):
        return b"not the uploaded bytes"


class FakeProcess:
    def __init__(self, returncode=0, stdout=b"", stderr=b""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr
        self.killed = False

    async def communicate(self):
        return self.stdout, self.stderr

    def kill(self):
        self.killed = True
        self.returncode = -9

    async def wait(self):
        return self.returncode


async def no_sleep(delay):
    return None


def make_store(tmp_path):
    now = datetime.now(timezone.utc)
    job_dir = tmp_path / VIDEO_ID
    job_dir.mkdir()
    source_path = job_dir / "original.mp4"
    segment_path = job_dir / "seg_00000.ts"
    source_path.write_bytes(b"source")
    segment_path.write_bytes(b"segment")

    return {
        "videos": {
            VIDEO_ID: {
                "id": VIDEO_ID,
                "title": "Robustness Video",
                "original_filename": "private-source.mp4",
                "description": "A robustness-test upload",
                "status": "awaiting_approval",
                "created_at": now,
                "updated_at": now,
                "manifest_address": None,
                "catalog_address": None,
                "error_message": None,
                "job_dir": str(job_dir),
                "job_source_path": str(source_path),
                "final_quote": {"storage_cost_atto": "123"},
                "final_quote_created_at": now,
                "approval_expires_at": now + timedelta(hours=1),
                "is_public": False,
                "show_original_filename": False,
                "show_manifest_address": False,
            }
        },
        "variants": [
            {
                "id": VARIANT_ID,
                "video_id": VIDEO_ID,
                "resolution": "720p",
                "width": 1280,
                "height": 720,
                "video_bitrate": 2_500_000,
                "audio_bitrate": 128_000,
                "segment_duration": 1.0,
                "total_duration": 1.0,
                "segment_count": 1,
            }
        ],
        "segments": [
            {
                "variant_id": VARIANT_ID,
                "segment_index": 0,
                "autonomi_address": None,
                "duration": 1.0,
                "byte_size": 7,
                "local_path": str(segment_path),
            }
        ],
    }


class AdminRobustnessTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="admin-robustness-"))
        self.store = make_store(self.tmp)
        self.scheduled_uploads = []
        self.originals = {
            "pool": admin.pool,
            "client": admin.AsyncAntdClient,
            "catalog_state_path": admin.CATALOG_STATE_PATH,
            "upload_temp_dir": admin.UPLOAD_TEMP_DIR,
            "schedule_upload_job": admin._schedule_upload_job,
            "upload_retries": admin.ANTD_UPLOAD_RETRIES,
            "upload_verify": admin.ANTD_UPLOAD_VERIFY,
            "upload_timeout": admin.ANTD_UPLOAD_TIMEOUT_SECONDS,
            "payment_mode": admin.ANTD_PAYMENT_MODE,
            "max_duration": admin.UPLOAD_MAX_DURATION_SECONDS,
        }
        FakeAntdClient.storage = {}
        FakeAntdClient.puts = 0
        MismatchingAntdClient.storage = {}
        MismatchingAntdClient.puts = 0
        admin.pool = FakePool(self.store)
        admin.AsyncAntdClient = FakeAntdClient
        admin.CATALOG_STATE_PATH = self.tmp / "catalog.json"
        admin.UPLOAD_TEMP_DIR = self.tmp
        admin._schedule_upload_job = self.scheduled_uploads.append
        admin.ANTD_UPLOAD_RETRIES = 2
        admin.ANTD_UPLOAD_VERIFY = True
        admin.ANTD_UPLOAD_TIMEOUT_SECONDS = 1
        admin.ANTD_PAYMENT_MODE = "auto"
        self.client = TestClient(admin.app)

    def tearDown(self):
        self.client.close()
        admin.pool = self.originals["pool"]
        admin.AsyncAntdClient = self.originals["client"]
        admin.CATALOG_STATE_PATH = self.originals["catalog_state_path"]
        admin.UPLOAD_TEMP_DIR = self.originals["upload_temp_dir"]
        admin._schedule_upload_job = self.originals["schedule_upload_job"]
        admin.ANTD_UPLOAD_RETRIES = self.originals["upload_retries"]
        admin.ANTD_UPLOAD_VERIFY = self.originals["upload_verify"]
        admin.ANTD_UPLOAD_TIMEOUT_SECONDS = self.originals["upload_timeout"]
        admin.ANTD_PAYMENT_MODE = self.originals["payment_mode"]
        admin.UPLOAD_MAX_DURATION_SECONDS = self.originals["max_duration"]
        shutil.rmtree(self.tmp, ignore_errors=True)

    def auth_header(self):
        response = self.client.post(
            "/auth/login",
            json={"username": "admin", "password": "admin"},
        )
        self.assertEqual(response.status_code, 200)
        return {"Authorization": f"Bearer {response.json()['access_token']}"}

    def test_put_public_verified_retries_until_mismatch_exhaustion(self):
        client = MismatchingAntdClient()

        async def run():
            with patch.object(admin.asyncio, "sleep", new=no_sleep):
                await admin._put_public_verified(client, b"payload", label="segment")

        with self.assertRaisesRegex(
            RuntimeError,
            "Autonomi upload failed verification for segment after 2 attempt",
        ):
            asyncio.run(run())

        self.assertEqual(MismatchingAntdClient.puts, 2)

    def test_upload_approved_video_records_autonomi_error_and_removes_job_dir(self):
        admin.AsyncAntdClient = MismatchingAntdClient
        self.store["videos"][VIDEO_ID]["status"] = "uploading"
        job_dir = Path(self.store["videos"][VIDEO_ID]["job_dir"])

        async def run():
            with patch.object(admin.asyncio, "sleep", new=no_sleep):
                await admin._upload_approved_video(VIDEO_ID)

        asyncio.run(run())

        row = self.store["videos"][VIDEO_ID]
        self.assertEqual(row["status"], "error")
        self.assertIn("Autonomi upload failed verification", row["error_message"])
        self.assertFalse(job_dir.exists())

    def test_corrupt_catalog_json_falls_back_to_empty_catalog(self):
        FakeAntdClient.storage["corrupt-catalog"] = b"{not json"
        admin._write_catalog_address("corrupt-catalog")

        response = self.client.get("/catalog", headers=self.auth_header())

        self.assertEqual(response.status_code, 200)
        body = response.json()
        self.assertEqual(body["catalog_address"], "corrupt-catalog")
        self.assertEqual(body["catalog"]["videos"], [])

    def test_approve_expired_quote_returns_410_and_deletes_local_files(self):
        row = self.store["videos"][VIDEO_ID]
        row["approval_expires_at"] = datetime.now(timezone.utc) - timedelta(seconds=1)
        job_dir = Path(row["job_dir"])

        response = self.client.post(
            f"/videos/{VIDEO_ID}/approve",
            headers=self.auth_header(),
        )

        self.assertEqual(response.status_code, 410)
        self.assertEqual(row["status"], "expired")
        self.assertIn("approval window expired", row["error_message"])
        self.assertFalse(job_dir.exists())
        self.assertEqual(self.scheduled_uploads, [])

    def test_approve_wrong_status_returns_409(self):
        self.store["videos"][VIDEO_ID]["status"] = "processing"

        response = self.client.post(
            f"/videos/{VIDEO_ID}/approve",
            headers=self.auth_header(),
        )

        self.assertEqual(response.status_code, 409)
        self.assertIn("not awaiting approval", response.json()["detail"])
        self.assertEqual(self.scheduled_uploads, [])

    def test_approve_missing_job_dir_returns_410(self):
        row = self.store["videos"][VIDEO_ID]
        shutil.rmtree(row["job_dir"], ignore_errors=True)

        response = self.client.post(
            f"/videos/{VIDEO_ID}/approve",
            headers=self.auth_header(),
        )

        self.assertEqual(response.status_code, 410)
        self.assertIn("Transcoded files are no longer available", response.json()["detail"])
        self.assertEqual(self.scheduled_uploads, [])

    def test_approve_empty_job_dir_returns_410(self):
        row = self.store["videos"][VIDEO_ID]
        shutil.rmtree(row["job_dir"], ignore_errors=True)
        row["job_dir"] = ""

        response = self.client.post(
            f"/videos/{VIDEO_ID}/approve",
            headers=self.auth_header(),
        )

        self.assertEqual(response.status_code, 410)
        self.assertIn("Transcoded files are no longer available", response.json()["detail"])
        self.assertEqual(self.scheduled_uploads, [])

    def test_upload_approved_video_missing_local_segment_sets_error(self):
        segment_path = Path(self.store["segments"][0]["local_path"])
        segment_path.unlink()
        self.store["videos"][VIDEO_ID]["status"] = "uploading"

        asyncio.run(admin._upload_approved_video(VIDEO_ID))

        row = self.store["videos"][VIDEO_ID]
        self.assertEqual(row["status"], "error")
        self.assertIn("Transcoded segment is missing from disk", row["error_message"])
        self.assertFalse(Path(self.store["videos"][VIDEO_ID]["job_dir"]).exists())

    def test_probe_upload_media_rejects_unreadable_video(self):
        async def fake_exec(*args, **kwargs):
            return FakeProcess(returncode=1, stderr=b"moov atom not found")

        with patch.object(admin.asyncio, "create_subprocess_exec", new=fake_exec):
            with self.assertRaises(HTTPException) as caught:
                asyncio.run(admin._probe_upload_media(self.tmp / "bad.mp4"))

        self.assertEqual(caught.exception.status_code, 400)
        self.assertIn("not a readable video", caught.exception.detail)
        self.assertIn("moov atom not found", caught.exception.detail)

    def test_probe_upload_media_rejects_invalid_json(self):
        async def fake_exec(*args, **kwargs):
            return FakeProcess(returncode=0, stdout=b"not-json")

        with patch.object(admin.asyncio, "create_subprocess_exec", new=fake_exec):
            with self.assertRaises(HTTPException) as caught:
                asyncio.run(admin._probe_upload_media(self.tmp / "bad.mp4"))

        self.assertEqual(caught.exception.status_code, 400)
        self.assertIn("invalid metadata", caught.exception.detail)

    def test_probe_upload_media_rejects_missing_video_stream(self):
        payload = {"streams": [{"codec_type": "audio"}], "format": {"duration": "1"}}

        async def fake_exec(*args, **kwargs):
            return FakeProcess(returncode=0, stdout=json.dumps(payload).encode())

        with patch.object(admin.asyncio, "create_subprocess_exec", new=fake_exec):
            with self.assertRaises(HTTPException) as caught:
                asyncio.run(admin._probe_upload_media(self.tmp / "audio.mp4"))

        self.assertEqual(caught.exception.status_code, 400)
        self.assertIn("does not contain a video stream", caught.exception.detail)

    def test_probe_upload_media_rejects_missing_dimensions(self):
        payload = {
            "streams": [{"codec_type": "video", "duration": "1"}],
            "format": {"duration": "1"},
        }

        async def fake_exec(*args, **kwargs):
            return FakeProcess(returncode=0, stdout=json.dumps(payload).encode())

        with patch.object(admin.asyncio, "create_subprocess_exec", new=fake_exec):
            with self.assertRaises(HTTPException) as caught:
                asyncio.run(admin._probe_upload_media(self.tmp / "nodims.mp4"))

        self.assertEqual(caught.exception.status_code, 400)
        self.assertIn("no usable dimensions", caught.exception.detail)

    def test_probe_upload_media_rejects_non_positive_dimensions(self):
        payload = {
            "streams": [
                {"codec_type": "video", "width": 0, "height": 720, "duration": "1"}
            ],
            "format": {"duration": "1"},
        }

        async def fake_exec(*args, **kwargs):
            return FakeProcess(returncode=0, stdout=json.dumps(payload).encode())

        with patch.object(admin.asyncio, "create_subprocess_exec", new=fake_exec):
            with self.assertRaises(HTTPException) as caught:
                asyncio.run(admin._probe_upload_media(self.tmp / "zerowidth.mp4"))

        self.assertEqual(caught.exception.status_code, 400)
        self.assertIn("invalid dimensions", caught.exception.detail)

    def test_probe_upload_media_rejects_duration_over_limit(self):
        admin.UPLOAD_MAX_DURATION_SECONDS = 10
        payload = {
            "streams": [
                {"codec_type": "video", "width": 1280, "height": 720, "duration": "11"}
            ],
            "format": {"duration": "11"},
        }

        async def fake_exec(*args, **kwargs):
            return FakeProcess(returncode=0, stdout=json.dumps(payload).encode())

        with patch.object(admin.asyncio, "create_subprocess_exec", new=fake_exec):
            with self.assertRaises(HTTPException) as caught:
                asyncio.run(admin._probe_upload_media(self.tmp / "long.mp4"))

        self.assertEqual(caught.exception.status_code, 413)
        self.assertIn("duration exceeds upload limit", caught.exception.detail)

    def test_run_ffmpeg_signal_9_error_explains_likely_memory_failure(self):
        async def fake_exec(*args, **kwargs):
            return FakeProcess(returncode=-9, stderr=b"killed")

        with patch.object(admin.asyncio, "create_subprocess_exec", new=fake_exec):
            with self.assertRaises(RuntimeError) as caught:
                asyncio.run(
                    admin._run_ffmpeg(
                        self.tmp / "source.mp4",
                        self.tmp / "segments",
                        640,
                        360,
                        500,
                        96,
                    )
                )

        message = str(caught.exception)
        self.assertIn("exit code -9", message)
        self.assertIn("killed by signal 9", message)
        self.assertIn("ran out of memory", message)
        self.assertIn("FFMPEG_THREADS", message)


if __name__ == "__main__":
    unittest.main()
