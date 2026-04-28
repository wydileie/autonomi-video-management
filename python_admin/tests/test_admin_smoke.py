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

os.environ.setdefault("ADMIN_DB_USER", "test")
os.environ.setdefault("ADMIN_DB_PASS", "test")
os.environ.setdefault("ADMIN_DB_HOST", "localhost")
os.environ.setdefault("ADMIN_DB_NAME", "test")
os.environ.setdefault("ADMIN_USERNAME", "admin")
os.environ.setdefault("ADMIN_PASSWORD", "admin")
os.environ.setdefault("ADMIN_AUTH_SECRET", "test-secret")
os.environ.setdefault("CORS_ALLOWED_ORIGINS", "http://localhost")

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from fastapi.testclient import TestClient

from src import admin_service as admin


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
        self.store = store
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
        video_id = str(args[-1])

        if query.startswith("UPDATE videos SET show_original_filename"):
            row = self.store["videos"].get(video_id)
            if not row:
                return None
            row["show_original_filename"] = args[0]
            row["show_manifest_address"] = args[1]
            row["updated_at"] = datetime.now(timezone.utc)
            return {"status": row["status"]}

        return self.store["videos"].get(video_id)

    async def fetch(self, query, *args):
        query = " ".join(query.split())
        if "FROM video_variants WHERE video_id=$1" in query or (
            "FROM video_variants" in query and "WHERE video_id=$1" in query
        ):
            video_id = str(args[0])
            return [
                row
                for row in self.store["variants"]
                if str(row["video_id"]) == video_id
            ]

        if "FROM video_segments WHERE variant_id=$1" in query or (
            "FROM video_segments" in query and "WHERE variant_id=$1" in query
        ):
            variant_id = str(args[0])
            return [
                row
                for row in self.store["segments"]
                if str(row["variant_id"]) == variant_id
            ]

        if "FROM videos" in query:
            return list(self.store["videos"].values())

        return []

    async def execute(self, query, *args):
        query = " ".join(query.split())
        if query.startswith("DELETE FROM videos WHERE id=$1"):
            video_id = str(args[0])
            existed = self.store["videos"].pop(video_id, None) is not None
            return "DELETE 1" if existed else "DELETE 0"

        if "SET status='uploading'" in query:
            self.store["videos"][str(args[0])]["status"] = "uploading"
            self.store["videos"][str(args[0])]["error_message"] = None
            return "UPDATE 1"

        if "SET status=$1" in query:
            status, error_message, video_id = args
            row = self.store["videos"].get(str(video_id))
            if row:
                row["status"] = status
                row["error_message"] = error_message
            return "UPDATE 1"

        if "SET status='ready'" in query:
            manifest_address, catalog_address, video_id = args
            row = self.store["videos"][str(video_id)]
            row["status"] = "ready"
            row["manifest_address"] = manifest_address
            row["catalog_address"] = catalog_address
            row["error_message"] = None
            row["approval_expires_at"] = None
            row["job_dir"] = None
            row["job_source_path"] = None
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

    async def data_cost(self, data):
        return SimpleNamespace(
            cost=str(len(data) * 10),
            file_size=len(data),
            chunk_count=max(1, (len(data) + 1023) // 1024),
            estimated_gas_cost_wei=str(len(data)),
            payment_mode="auto",
        )

    async def data_put_public(self, data, payment_mode="auto"):
        self.__class__.puts += 1
        address = f"addr-{self.__class__.puts}"
        self.__class__.storage[address] = data
        return SimpleNamespace(
            address=address,
            chunks_stored=1,
            payment_mode_used=payment_mode,
            cost=str(len(data) * 10),
        )

    async def data_get_public(self, address):
        return self.__class__.storage[address]


def make_store(tmp_path):
    now = datetime.now(timezone.utc)
    video_id = "11111111-1111-1111-1111-111111111111"
    variant_id = "22222222-2222-2222-2222-222222222222"
    job_dir = tmp_path / video_id
    job_dir.mkdir()
    (job_dir / "seg_00000.ts").write_bytes(b"segment")

    return {
        "videos": {
            video_id: {
                "id": video_id,
                "title": "Smoke Video",
                "original_filename": "private-source.mp4",
                "description": "A smoke-test upload",
                "status": "awaiting_approval",
                "created_at": now,
                "updated_at": now,
                "manifest_address": None,
                "catalog_address": None,
                "error_message": None,
                "job_dir": str(job_dir),
                "job_source_path": str(job_dir / "original.mp4"),
                "final_quote": {"storage_cost_atto": "123"},
                "final_quote_created_at": now,
                "approval_expires_at": now + timedelta(hours=1),
                "show_original_filename": False,
                "show_manifest_address": False,
            }
        },
        "variants": [
            {
                "id": variant_id,
                "video_id": video_id,
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
                "variant_id": variant_id,
                "segment_index": 0,
                "autonomi_address": "segment-addr",
                "duration": 1.0,
                "byte_size": 7,
                "local_path": str(job_dir / "seg_00000.ts"),
            }
        ],
    }


class AdminSmokeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="admin-smoke-"))
        self.store = make_store(self.tmp)
        self.scheduled_uploads = []
        self.originals = {
            "pool": admin.pool,
            "client": admin.AsyncAntdClient,
            "catalog_state_path": admin.CATALOG_STATE_PATH,
            "upload_temp_dir": admin.UPLOAD_TEMP_DIR,
            "schedule_upload_job": admin._schedule_upload_job,
        }
        FakeAntdClient.storage = {}
        FakeAntdClient.puts = 0
        admin.pool = FakePool(self.store)
        admin.AsyncAntdClient = FakeAntdClient
        admin.CATALOG_STATE_PATH = self.tmp / "catalog.json"
        admin.UPLOAD_TEMP_DIR = self.tmp
        admin._schedule_upload_job = self.scheduled_uploads.append
        self.client = TestClient(admin.app)

    def tearDown(self):
        self.client.close()
        admin.pool = self.originals["pool"]
        admin.AsyncAntdClient = self.originals["client"]
        admin.CATALOG_STATE_PATH = self.originals["catalog_state_path"]
        admin.UPLOAD_TEMP_DIR = self.originals["upload_temp_dir"]
        admin._schedule_upload_job = self.originals["schedule_upload_job"]
        shutil.rmtree(self.tmp, ignore_errors=True)

    def auth_header(self):
        response = self.client.post(
            "/auth/login",
            json={"username": "admin", "password": "admin"},
        )
        self.assertEqual(response.status_code, 200)
        return {"Authorization": f"Bearer {response.json()['access_token']}"}

    def test_auth_login_and_me_smoke(self):
        response = self.client.post(
            "/auth/login",
            json={"username": "admin", "password": "bad"},
        )
        self.assertEqual(response.status_code, 401)

        headers = self.auth_header()
        response = self.client.get("/auth/me", headers=headers)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), {"username": "admin"})

    def test_quote_requires_auth_and_returns_upload_estimate(self):
        payload = {
            "duration_seconds": 2.1,
            "resolutions": ["720p"],
            "source_width": 1920,
            "source_height": 1080,
        }
        unauthenticated = self.client.post("/videos/upload/quote", json=payload)
        self.assertEqual(unauthenticated.status_code, 401)

        response = self.client.post(
            "/videos/upload/quote",
            json=payload,
            headers=self.auth_header(),
        )
        self.assertEqual(response.status_code, 200)
        quote = response.json()
        self.assertEqual(quote["segment_duration"], 1.0)
        self.assertEqual(quote["segment_count"], 3)
        self.assertEqual(quote["variants"][0]["resolution"], "720p")
        self.assertGreater(int(quote["storage_cost_atto"]), 0)

    def test_approval_moves_video_to_uploading_and_status_reflects_transition(self):
        video_id = next(iter(self.store["videos"]))

        before = self.client.get(f"/videos/{video_id}/status")
        self.assertEqual(before.status_code, 200)
        self.assertEqual(before.json()["status"], "awaiting_approval")

        response = self.client.post(
            f"/videos/{video_id}/approve",
            headers=self.auth_header(),
        )
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["status"], "uploading")
        self.assertEqual(self.scheduled_uploads, [video_id])

        after = self.client.get(f"/videos/{video_id}/status")
        self.assertEqual(after.status_code, 200)
        self.assertEqual(after.json()["status"], "uploading")

    def test_manifest_generation_redacts_private_fields_until_visibility_enabled(self):
        video_id = next(iter(self.store["videos"]))
        self.store["videos"][video_id]["status"] = "ready"

        manifest = asyncio.run(admin._build_ready_manifest_from_db(video_id))
        self.assertIsNone(manifest["original_filename"])
        self.assertFalse(manifest["show_manifest_address"])
        self.assertEqual(
            manifest["variants"][0]["segments"][0]["autonomi_address"],
            "segment-addr",
        )

        response = self.client.patch(
            f"/admin/videos/{video_id}/visibility",
            json={"show_original_filename": True, "show_manifest_address": True},
            headers=self.auth_header(),
        )
        self.assertEqual(response.status_code, 200)
        body = response.json()
        self.assertTrue(body["show_original_filename"])
        self.assertTrue(body["show_manifest_address"])
        self.assertIsNotNone(body["manifest_address"])

        catalog_address = body["catalog_address"]
        catalog = json.loads(FakeAntdClient.storage[catalog_address].decode("utf-8"))
        public_entry = catalog["videos"][0]
        self.assertEqual(public_entry["original_filename"], "private-source.mp4")
        self.assertEqual(public_entry["manifest_address"], body["manifest_address"])

    def test_public_catalog_hides_manifest_and_delete_removes_video(self):
        video_id = next(iter(self.store["videos"]))
        self.store["videos"][video_id]["status"] = "ready"
        manifest = asyncio.run(admin._build_ready_manifest_from_db(video_id))
        entry = admin._video_catalog_entry(manifest, "manifest-hidden")
        catalog = {
            "schema_version": 1,
            "content_type": admin.CATALOG_CONTENT_TYPE,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "videos": [entry],
        }
        FakeAntdClient.storage["catalog-visible"] = json.dumps(catalog).encode("utf-8")
        admin._write_catalog_address("catalog-visible")

        response = self.client.get("/videos")
        self.assertEqual(response.status_code, 200)
        videos = response.json()
        self.assertEqual(len(videos), 1)
        self.assertIsNone(videos[0]["original_filename"])
        self.assertIsNone(videos[0]["manifest_address"])

        response = self.client.delete(
            f"/videos/{video_id}",
            headers=self.auth_header(),
        )
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["deleted"], video_id)
        self.assertNotIn(video_id, self.store["videos"])
        self.assertFalse((self.tmp / video_id).exists())

        new_catalog_address = response.json()["catalog_address"]
        new_catalog = json.loads(FakeAntdClient.storage[new_catalog_address].decode("utf-8"))
        self.assertEqual(new_catalog["videos"], [])


if __name__ == "__main__":
    unittest.main()
