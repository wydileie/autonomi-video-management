import json
import os
import shutil
import sys
import tempfile
import unittest
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

import asyncpg

os.environ.setdefault("ADMIN_DB_USER", "test")
os.environ.setdefault("ADMIN_DB_PASS", "test")
os.environ.setdefault("ADMIN_DB_HOST", "localhost")
os.environ.setdefault("ADMIN_DB_NAME", "test")
os.environ.setdefault("ADMIN_USERNAME", "admin")
os.environ.setdefault("ADMIN_PASSWORD", "admin")
os.environ.setdefault("ADMIN_AUTH_SECRET", "test-secret")
os.environ.setdefault("CORS_ALLOWED_ORIGINS", "http://localhost")

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from src import admin_service as admin


POSTGRES_TEST_DSN = os.environ.get("PYTHON_ADMIN_POSTGRES_TEST_DSN") or os.environ.get(
    "ADMIN_POSTGRES_TEST_DSN"
)


@unittest.skipUnless(
    POSTGRES_TEST_DSN,
    "set PYTHON_ADMIN_POSTGRES_TEST_DSN or ADMIN_POSTGRES_TEST_DSN "
    "to run real Postgres integration tests",
)
class AdminPostgresIntegrationTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.schema = f"admin_it_{uuid.uuid4().hex}"
        self.temp_dirs: list[Path] = []
        self.original_pool = admin.pool
        self.original_schedule_processing_job = admin._schedule_processing_job
        self.original_schedule_upload_job = admin._schedule_upload_job

        setup_conn = await asyncpg.connect(POSTGRES_TEST_DSN)
        try:
            await setup_conn.execute(f'CREATE SCHEMA "{self.schema}"')
        finally:
            await setup_conn.close()

        self.pool = await asyncpg.create_pool(
            POSTGRES_TEST_DSN,
            min_size=1,
            max_size=2,
            server_settings={"search_path": f"{self.schema},public"},
        )
        admin.pool = self.pool

    async def asyncTearDown(self):
        admin.pool = self.original_pool
        admin._schedule_processing_job = self.original_schedule_processing_job
        admin._schedule_upload_job = self.original_schedule_upload_job

        await self.pool.close()

        teardown_conn = await asyncpg.connect(POSTGRES_TEST_DSN)
        try:
            await teardown_conn.execute(f'DROP SCHEMA IF EXISTS "{self.schema}" CASCADE')
        finally:
            await teardown_conn.close()

        for temp_dir in self.temp_dirs:
            shutil.rmtree(temp_dir, ignore_errors=True)

    def make_temp_dir(self, prefix: str) -> Path:
        temp_dir = Path(tempfile.mkdtemp(prefix=prefix))
        self.temp_dirs.append(temp_dir)
        return temp_dir

    async def test_schema_insert_list_delete_and_cascade(self):
        await admin._ensure_schema()
        video_id = uuid.uuid4()

        async with self.pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO videos (
                    id, title, original_filename, description, status,
                    requested_resolutions, final_quote,
                    show_original_filename, show_manifest_address, user_id
                )
                VALUES ($1, $2, $3, $4, 'ready', $5::jsonb, $6::jsonb, $7, $8, $9)
                """,
                video_id,
                "Integration Video",
                "integration-source.mp4",
                "Created by a real Postgres integration test",
                json.dumps(["720p"]),
                json.dumps({"storage_cost_atto": "123"}),
                True,
                False,
                "integration-admin",
            )
            variant_id = await conn.fetchval(
                """
                INSERT INTO video_variants (
                    video_id, resolution, width, height, video_bitrate,
                    audio_bitrate, segment_duration, total_duration, segment_count
                )
                VALUES ($1, '720p', 1280, 720, 2500000, 128000, 1.0, 2.0, 2)
                RETURNING id
                """,
                video_id,
            )
            await conn.execute(
                """
                INSERT INTO video_segments (
                    variant_id, segment_index, autonomi_address, duration,
                    byte_size, local_path
                )
                VALUES
                    ($1, 0, 'segment-address-0', 1.0, 10, '/tmp/seg_00000.ts'),
                    ($1, 1, 'segment-address-1', 1.0, 11, '/tmp/seg_00001.ts')
                """,
                variant_id,
            )

        listed = await admin.admin_list_videos(username="integration-admin")
        self.assertEqual([video.id for video in listed], [str(video_id)])
        self.assertEqual(listed[0].variants[0].resolution, "720p")

        detail = await admin._get_db_video(str(video_id), include_segments=True)
        self.assertEqual(detail.variants[0].segment_count, 2)
        self.assertEqual(
            [segment.autonomi_address for segment in detail.variants[0].segments],
            ["segment-address-0", "segment-address-1"],
        )

        async with self.pool.acquire() as conn:
            await conn.execute("DELETE FROM videos WHERE id=$1", video_id)
            remaining_variants = await conn.fetchval("SELECT count(*) FROM video_variants")
            remaining_segments = await conn.fetchval("SELECT count(*) FROM video_segments")

        self.assertEqual(remaining_variants, 0)
        self.assertEqual(remaining_segments, 0)

    async def test_cleanup_expired_approvals_marks_rows_and_removes_job_dir(self):
        await admin._ensure_schema()
        expired_video_id = uuid.uuid4()
        active_video_id = uuid.uuid4()
        expired_job_dir = self.make_temp_dir("admin-pg-expired-")
        active_job_dir = self.make_temp_dir("admin-pg-active-")
        (expired_job_dir / "seg_00000.ts").write_bytes(b"expired")
        (active_job_dir / "seg_00000.ts").write_bytes(b"active")

        async with self.pool.acquire() as conn:
            await conn.executemany(
                """
                INSERT INTO videos (
                    id, title, original_filename, status, job_dir,
                    approval_expires_at
                )
                VALUES ($1, $2, $3, 'awaiting_approval', $4, $5)
                """,
                [
                    (
                        expired_video_id,
                        "Expired Quote",
                        "expired.mp4",
                        str(expired_job_dir),
                        datetime.now(timezone.utc) - timedelta(seconds=1),
                    ),
                    (
                        active_video_id,
                        "Active Quote",
                        "active.mp4",
                        str(active_job_dir),
                        datetime.now(timezone.utc) + timedelta(hours=1),
                    ),
                ],
            )

        await admin._cleanup_expired_approvals()

        async with self.pool.acquire() as conn:
            expired = await conn.fetchrow(
                "SELECT status, error_message FROM videos WHERE id=$1",
                expired_video_id,
            )
            active = await conn.fetchrow(
                "SELECT status, error_message FROM videos WHERE id=$1",
                active_video_id,
            )

        self.assertEqual(expired["status"], "expired")
        self.assertIn("approval window expired", expired["error_message"])
        self.assertFalse(expired_job_dir.exists())
        self.assertEqual(active["status"], "awaiting_approval")
        self.assertIsNone(active["error_message"])
        self.assertTrue(active_job_dir.exists())

    async def test_recover_interrupted_jobs_schedules_recoverable_work(self):
        await admin._ensure_schema()
        recoverable_video_id = uuid.uuid4()
        uploading_video_id = uuid.uuid4()
        broken_video_id = uuid.uuid4()
        recoverable_job_dir = self.make_temp_dir("admin-pg-recoverable-")
        source_path = recoverable_job_dir / "original_source.mp4"
        source_path.write_bytes(b"source")

        processing_jobs = []
        upload_jobs = []

        def record_processing_job(video_id, src_path, resolutions, job_dir, *, reset_existing=False):
            processing_jobs.append((video_id, src_path, resolutions, job_dir, reset_existing))

        admin._schedule_processing_job = record_processing_job
        admin._schedule_upload_job = upload_jobs.append

        async with self.pool.acquire() as conn:
            await conn.executemany(
                """
                INSERT INTO videos (
                    id, title, original_filename, status, job_dir,
                    job_source_path, requested_resolutions
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)
                """,
                [
                    (
                        recoverable_video_id,
                        "Recoverable Processing",
                        "recoverable.mp4",
                        "pending",
                        str(recoverable_job_dir),
                        str(source_path),
                        json.dumps(["720p"]),
                    ),
                    (
                        uploading_video_id,
                        "Recoverable Upload",
                        "uploading.mp4",
                        "uploading",
                        str(recoverable_job_dir),
                        str(source_path),
                        json.dumps(["720p"]),
                    ),
                    (
                        broken_video_id,
                        "Broken Processing",
                        "broken.mp4",
                        "processing",
                        str(recoverable_job_dir / "missing"),
                        str(recoverable_job_dir / "missing" / "original.mp4"),
                        json.dumps(["720p"]),
                    ),
                ],
            )

        await admin._recover_interrupted_jobs()

        self.assertEqual(len(processing_jobs), 1)
        video_id, src_path, resolutions, job_dir, reset_existing = processing_jobs[0]
        self.assertEqual(video_id, str(recoverable_video_id))
        self.assertEqual(src_path, source_path)
        self.assertEqual(resolutions, ["720p"])
        self.assertEqual(job_dir, recoverable_job_dir)
        self.assertTrue(reset_existing)
        self.assertEqual(upload_jobs, [str(uploading_video_id)])

        async with self.pool.acquire() as conn:
            broken = await conn.fetchrow(
                "SELECT status, error_message FROM videos WHERE id=$1",
                broken_video_id,
            )
        self.assertEqual(broken["status"], "error")
        self.assertIn("could not be recovered", broken["error_message"])


if __name__ == "__main__":
    unittest.main()
