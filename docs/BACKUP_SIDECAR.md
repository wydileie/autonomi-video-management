# Backup Sidecar

`docker-compose.backup.yml` adds an opt-in `backup_sidecar` service for
scheduled production backups. It is not part of the base or production Compose
files; include the override only on hosts where you want automated backups.

The sidecar writes the same core artifacts as `scripts/backup-production.sh`:

- `postgres.dump`: a custom-format `pg_dump` of `ADMIN_DB`
- `catalog.json`: the latest catalog bookmark when `/catalog/catalog.json`
  exists
- `manifest.env`: backup metadata without database passwords
- `SHA256SUMS`: checksums for the files in the backup directory

Ready video data is stored on Autonomi. This sidecar backs up the local
Postgres state and the latest catalog bookmark, not the temporary processing
directory.

## Quick Start

Create a private host directory for backups:

```bash
sudo mkdir -p /srv/autonomi-video-management/backups
sudo chmod 700 /srv/autonomi-video-management/backups
```

Start production with the backup override:

```bash
BACKUP_HOST_PATH=/srv/autonomi-video-management/backups \
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.backup.yml \
  up --build -d
```

By default the sidecar runs once per day at `02:00` UTC and deletes matching
backup directories older than 14 days.

To trigger a one-shot backup without waiting for the schedule:

```bash
BACKUP_HOST_PATH=/srv/autonomi-video-management/backups \
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.backup.yml \
  run --rm backup_sidecar run
```

## Configuration

Set these values in the shell, `.env.production`, or an uncommitted Compose
override.

| Variable | Default | Notes |
| --- | --- | --- |
| `BACKUP_HOST_PATH` | `./backups` | Host directory mounted at `/backups`. Prefer an absolute path in production. |
| `BACKUP_PREFIX` | `autvid` | Backup directories are named `${BACKUP_PREFIX}-YYYYMMDDTHHMMSSZ`. |
| `BACKUP_SCHEDULE` | `daily@02:00` | Use `daily@HH:MM` in UTC, `interval:SECONDS`, or `once`. |
| `BACKUP_RUN_ON_START` | `false` | Set to `true` to create a backup immediately when the service starts. |
| `BACKUP_RETENTION_DAYS` | `14` | Deletes matching backup directories older than this many days. Set `0` to disable. |
| `BACKUP_RETENTION_COUNT` | `0` | Keeps only the newest N matching backup directories. Set `0` to disable. |
| `BACKUP_CATALOG` | `true` | Set `false` to skip the catalog bookmark copy. |
| `BACKUP_FILE_OWNER` | unset | Optional numeric `UID:GID` to apply to completed backup directories. |
| `BACKUP_DB_WAIT_SECONDS` | `120` | How long each run waits for Postgres readiness. |
| `BACKUP_TEXTFILE_HOST_PATH` | `./monitoring/textfile` | Host directory shared with node-exporter for backup Prometheus textfile metrics. |
| `BACKUP_TEXTFILE_DIR` | `/var/lib/node_exporter/textfile_collector` | In-container textfile collector directory. Leave empty to disable metric emission. |
| `BACKUP_TEXTFILE_NAME` | `autvid_backup.prom` | Metric filename written atomically after each backup attempt. |

The sidecar receives `POSTGRES_DB` from `ADMIN_DB` and uses the read-only
database role configured by `BACKUP_DB_USER` and `BACKUP_DB_PASS`. New
Postgres volumes create that role during database initialization. Do not commit
real `.env.production` files or backup artifacts.

For secret-file based database passwords, set `BACKUP_DB_PASS_FILE` in Compose
or mount a file and set `POSTGRES_PASSWORD_FILE` inside `backup_sidecar`. When
`POSTGRES_PASSWORD_FILE` is set, it wins over `PGPASSWORD`.

## Metrics

When `BACKUP_TEXTFILE_DIR` is set, the sidecar writes Prometheus textfile
metrics after every backup attempt. The monitoring override starts
`node_exporter` with its textfile collector pointed at the same
`BACKUP_TEXTFILE_HOST_PATH`, and Prometheus scrapes `node_exporter:9100`.

The emitted metrics include:

- `autvid_backup_last_success_timestamp_seconds`
- `autvid_backup_last_run_timestamp_seconds`
- `autvid_backup_last_duration_seconds`
- `autvid_backup_last_size_bytes`
- `autvid_backup_last_status{status="success|failure"}`
- `autvid_backup_retained_count`

`AutvidBackupMissing`, `AutvidBackupFailed`, and `AutvidBackupStale` alerts are
defined in `monitoring/prometheus/rules/autvid-alerts.yml`.

## Restore

Sidecar backup directories are compatible with the existing restore helper:

```bash
scripts/restore-production.sh \
  --backup-dir /srv/autonomi-video-management/backups/autvid-YYYYMMDDTHHMMSSZ \
  --yes
```

The restore script is destructive: it runs `pg_restore --clean --if-exists`
against `ADMIN_DB` and overwrites the catalog bookmark when `catalog.json` is
present. See `scripts/restore-production.sh --help` for explicit `--db-file`
and `--catalog-file` restore modes.

## Operations

Check sidecar status and logs:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.backup.yml \
  ps backup_sidecar

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.backup.yml \
  logs -f backup_sidecar
```

Stop only the sidecar:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.backup.yml \
  stop backup_sidecar
```
