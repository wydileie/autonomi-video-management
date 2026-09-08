"""Regression checks for media archive inputs and coordinated state restore."""
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import sqlite3
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("media_archive", ROOT / "scripts/verify-media-archive.py")
media = importlib.util.module_from_spec(spec)
spec.loader.exec_module(media)


class MediaArchiveTests(unittest.TestCase):
    def make_archive(self, path, entries):
        with tarfile.open(path, "w:gz") as bundle:
            for name, kind in entries:
                item = tarfile.TarInfo(name)
                item.type = kind
                item.size = 3 if kind == tarfile.REGTYPE else 0
                item.linkname = "/etc/passwd" if kind == tarfile.SYMTYPE else ""
                bundle.addfile(item, io.BytesIO(b"bin") if item.size else None)
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def test_extracts_only_verified_regular_binaries(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            archive = base / "tools.tgz"
            digest = self.make_archive(archive, [("ffmpeg", tarfile.REGTYPE), ("./ffprobe", tarfile.REGTYPE)])
            media.extract(archive, digest, base / "out")
            self.assertEqual((base / "out/ffmpeg").read_bytes(), b"bin")
            self.assertEqual((base / "out/ffprobe").stat().st_mode & 0o777, 0o755)
            with self.assertRaises(ValueError):
                media.extract(archive, "0" * 64, base / "wrong")
            self.assertFalse((base / "wrong").exists())

    def test_rejects_traversal_links_duplicates_and_missing_tools(self):
        for invalid in [[("../ffmpeg", tarfile.REGTYPE)], [("ffmpeg", tarfile.SYMTYPE)],
                        [("ffmpeg", tarfile.REGTYPE)] * 2, [("ffprobe", tarfile.REGTYPE)]]:
            with self.subTest(invalid=invalid), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                archive = base / "tools.tgz"
                digest = self.make_archive(archive, invalid)
                with self.assertRaises(ValueError):
                    media.extract(archive, digest, base / "out")
                self.assertFalse((base / "out").exists())


class StateBackupTests(unittest.TestCase):
    def test_backup_restore_preserves_state_and_pauses_all_signing(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            source = base / "source"
            (source / "catalog").mkdir(parents=True)
            (source / "catalog/catalog.json").write_text('{"catalog_address":"published"}')
            with sqlite3.connect(source / "autvid.sqlite3") as db:
                db.execute("CREATE TABLE example(value TEXT)")
                db.execute("INSERT INTO example VALUES('preserved')")
            with sqlite3.connect(source / "antd-payments.sqlite3") as db:
                db.execute("CREATE TABLE payment_approvals(id TEXT,state TEXT)")
                db.execute("INSERT INTO payment_approvals VALUES('original','open')")
            env = dict(os.environ, AUTVID_DATA_HOST_PATH=str(source), ANTD_PAYMENT_DB_PATH=str(source / "antd-payments.sqlite3"))
            subprocess.run([str(ROOT / 'scripts/backup-production.sh'), '--output-dir', str(base / 'backups'), '--timestamp', 'test'], env=env, check=True, capture_output=True)
            backup = base / 'backups/autvid-test'
            target = base / 'restore'
            env.update(AUTVID_DATA_HOST_PATH=str(target), ANTD_PAYMENT_DB_PATH=str(target / 'antd-payments.sqlite3'))
            subprocess.run([str(ROOT / 'scripts/restore-production.sh'), '--backup-dir', str(backup), '--yes'], env=env, check=True, capture_output=True)
            with sqlite3.connect(target / 'autvid.sqlite3') as db:
                self.assertEqual(db.execute('SELECT value FROM example').fetchone()[0], 'preserved')
            with sqlite3.connect(target / 'antd-payments.sqlite3') as db:
                self.assertEqual(db.execute('SELECT state FROM payment_approvals').fetchone()[0], 'paused')
                self.assertEqual(db.execute("SELECT value FROM payment_controls WHERE key='restore_reconciliation_required'").fetchone()[0], '1')
            self.assertEqual((target / 'antd-payments.sqlite3').stat().st_mode & 0o777, 0o600)
            # Missing payment material must fail before modifying the restored admin DB.
            before = (target / 'autvid.sqlite3').read_bytes()
            (backup / 'antd-payments.sqlite3').unlink()
            result = subprocess.run([str(ROOT / 'scripts/restore-production.sh'), '--backup-dir', str(backup), '--yes'], env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual((target / 'autvid.sqlite3').read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
