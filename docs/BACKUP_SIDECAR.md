# Backup Sidecar

`docker-compose.backup.yml` adds an opt-in `backup_sidecar` service for
scheduled production backups. It is not part of the base or production Compose
files; include the override only on hosts where you want automated backups.

The sidecar writes the same core artifacts as `scripts/backup-production.sh`:

- `autvid.sqlite3`: SQLite database backup created with SQLite's online backup command
- `catalog.json`: current catalog state when present
- `manifest.env`: backup metadata
- `SHA256SUMS`: checksums for backup artifacts

Ready video bytes and full playback manifests are stored on Autonomi. This
sidecar backs up local admin state, durable jobs, auth sessions, and catalog
bookmarks from the app-data directory.

## Quick Start

```bash
sudo mkdir -p /srv/autonomi-video-management/backups
sudo chmod 700 /srv/autonomi-video-management/backups

BACKUP_HOST_PATH=/srv/autonomi-video-management/backups \
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.backup.yml \
  up --build -d
```

Run a one-shot backup:

```bash
BACKUP_HOST_PATH=/srv/autonomi-video-management/backups \
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.backup.yml \
  run --rm backup_sidecar run
```

## Configuration

| Variable | Default | Notes |
| --- | --- | --- |
| `AUTVID_DATA_HOST_PATH` | `./.autvid/app_data` | Host app-data directory mounted read-only into the sidecar. |
| `BACKUP_HOST_PATH` | `./backups` | Host directory mounted at `/backups`. Prefer an absolute path in production. |
| `BACKUP_PREFIX` | `autvid` | Backup directories are named `${BACKUP_PREFIX}-YYYYMMDDTHHMMSSZ`. |
| `BACKUP_SCHEDULE` | `daily@02:00` | Use `daily@HH:MM` in UTC, `interval:SECONDS`, or `once`. |
| `BACKUP_RUN_ON_START` | `false` | Set to `true` to create a backup immediately when the service starts. |
| `BACKUP_RETENTION_DAYS` | `14` | Deletes matching backup directories older than this many days. Set `0` to disable. |
| `BACKUP_RETENTION_COUNT` | `0` | Keeps only the newest N matching backup directories. Set `0` to disable. |
| `BACKUP_CATALOG` | `true` | Set `false` to skip the catalog state copy. |
| `BACKUP_SQLITE_WAIT_SECONDS` | `120` | How long each run waits for the SQLite database file to exist. |
| `BACKUP_DB_WAIT_SECONDS` | unset | Legacy alias used only when `BACKUP_SQLITE_WAIT_SECONDS` is unset. |
| `BACKUP_TEXTFILE_HOST_PATH` | `./monitoring/textfile` | Host directory shared with node-exporter for backup Prometheus textfile metrics. |
| `BACKUP_TEXTFILE_DIR` | `/var/lib/node_exporter/textfile_collector` | In-container textfile collector directory. Leave empty to disable metric emission. |
| `BACKUP_TEXTFILE_NAME` | `autvid_backup.prom` | Metric filename written atomically after each backup attempt. |

## Restore

```bash
scripts/restore-production.sh \
  --backup-dir /srv/autonomi-video-management/backups/autvid-YYYYMMDDTHHMMSSZ \
  --yes
```

Stop the stack before restoring so SQLite sidecar files are not in use. The
restore script replaces `autvid.sqlite3`, removes stale WAL/SHM sidecars when
the backup does not contain legacy copies of them, and overwrites `catalog.json`
when the backup contains one.
