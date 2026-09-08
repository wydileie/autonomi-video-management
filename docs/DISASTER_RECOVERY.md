# Disaster Recovery

Ready video bytes and full playback manifests are stored on Autonomi. Local
recovery focuses on the app-data directory:

- `antd-payments.sqlite3`: gateway approvals, signing reservations, transactions, and receipts.
- `autvid.sqlite3`: video rows, job state, auth sessions, and local metadata.
- `catalog/catalog.json`: latest published and all-videos catalog addresses and snapshots.
- `processing/`: only needed for uploads that are still processing, awaiting approval, or uploading.
- `.env.production`: recreate from `.env.production.example` and restore real secrets from a secret manager.

The backup sidecar and `scripts/backup-production.sh` capture both SQLite databases and catalog
state. See `docs/BACKUP_SIDECAR.md` for scheduled backups.

## Restore To A New Host

1. Install Docker and clone the repository.
2. Create `.env.production` from `.env.production.example`.
3. Restore wallet, admin auth, domain, and `AUTVID_DATA_HOST_PATH`.
4. Copy the chosen backup directory to the host.
5. Stop the stack if it is running.
6. Restore the backup:

```bash
ANTD_PAYMENT_DB_PATH=/srv/autonomi-video-management/gateway/antd-payments.sqlite3 \
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

## Restore With Known Catalog Addresses

If local app data is unavailable but you know the latest catalog addresses, set
them in `.env.production` and start the stack:

```dotenv
PUBLISHED_CATALOG_ADDRESS=<published-catalog-address>
ALL_CATALOG_ADDRESS=<all-videos-catalog-address>
```

This restores portable playback discovery for viewer applications that know the
addresses. It does not recover admin auth sessions, approval state, or job
history.

The journal path must target the gateway's actual persistent volume or native
app-data file. For Docker named volumes, mount the volume in a maintenance
container to restore the journal while the gateway is stopped. Never restore
only admin state and silently create a fresh journal for the same wallet.
The restore sets a signing pause pending reconciliation; read/playback checks
can run while writes remain paused. See `PAYMENT_RECOVERY.md`.

Production Compose repairs restored gateway journal ownership before starting
`antd`: the payment directory and journal belong to UID/GID `10001:10001`, with
modes 700 and 600. The existing admin initializer uses `1000:1000`. A restore
performed as root is supported through these startup initializers. For native
launches, keep both databases owned by the account that runs the application.
