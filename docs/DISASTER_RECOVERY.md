# Disaster Recovery

Ready video data is stored on Autonomi. Local recovery focuses on Postgres,
the latest catalog bookmark, and the processing directory used by jobs that
were not ready yet.

## What To Back Up

- Postgres database: video rows, job state, auth data, and catalog metadata.
- Catalog bookmark: `/catalog/catalog.json` in the `catalog_state` volume.
- Processing bind mount: `VIDEO_PROCESSING_HOST_PATH`, only needed for uploads
  that are still processing or awaiting approval.
- `.env.production`: recreate from `.env.production.example` and restore real
  secrets from a secret manager, not from Git.

The backup sidecar and `scripts/backup-production.sh` capture Postgres and the
catalog bookmark. See `docs/BACKUP_SIDECAR.md` for scheduled backups.

## Restore To A New Host

1. Install Docker and clone the repository.
2. Create `.env.production` from `.env.production.example`.
3. Restore wallet, database, admin auth, domain, and processing path values.
4. Copy the chosen backup directory to the host.
5. Start only Postgres and supporting volumes:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  up -d db init_permissions
```

6. Restore the backup:

```bash
scripts/restore-production.sh \
  --backup-dir /srv/autonomi-video-management/backups/autvid-YYYYMMDDTHHMMSSZ \
  --yes
```

7. Start the full stack:

```bash
make up-prod
```

8. Verify health and catalog visibility:

```bash
curl http://localhost/api/health
curl http://localhost/stream/health
curl http://localhost/api/videos
```

## Restore With A Known Catalog Address

If Postgres is unavailable but you know the latest catalog address, set it in
`.env.production` and start the stack:

```dotenv
CATALOG_ADDRESS=<latest-catalog-address>
```

This can restore public playback discovery while admin metadata is rebuilt or
restored separately. It does not recover admin users, approval state, or job
history.

## Failed Restore

If `pg_restore` fails because the database is in use, stop app services and
leave only Postgres running:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  stop rust_admin rust_stream react_frontend nginx antd
```

Run the restore again. If the catalog bookmark is wrong, restore only the
catalog file from a known-good backup:

```bash
scripts/restore-production.sh \
  --catalog-file /srv/autonomi-video-management/backups/autvid-YYYYMMDDTHHMMSSZ/catalog.json \
  --yes
```

After recovery, create a fresh backup and record the catalog address shown in
the UI or stream logs.
