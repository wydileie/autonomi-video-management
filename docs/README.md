# Documentation

Start with the root [README](../README.md) for the quick start, service list,
environment variables, and API reference. The docs in this directory go deeper
on operational and architectural topics.

## Architecture

- [Architecture](ARCHITECTURE.md) explains the service boundaries, durable job
  flow, catalog state model, and reliability decisions.
- [Runtime modes](RUNTIME_MODES.md) documents the Compose and standalone runtime
  contracts.
- [Runtime contract example](runtime-contract.example.json) shows the
  machine-readable endpoint and path contract used by native hosts.

## Operations

- [Deployment](DEPLOYMENT.md) covers local, public-demo, and production Compose
  modes.
- [Observability](OBSERVABILITY.md) covers Prometheus, Grafana, Alertmanager,
  Loki, and Promtail.
- [Alertmanager setup](ALERTMANAGER_SETUP.md) shows how to replace the default
  no-op alert receiver.
- [Backup sidecar](BACKUP_SIDECAR.md) covers scheduled SQLite and catalog-state
  backups.
- [Disaster recovery](DISASTER_RECOVERY.md) covers recovery from backups and
  network-hosted catalog documents.
- [Troubleshooting](TROUBLESHOOTING.md) collects common local and production
  failure modes.
- [Performance tuning](PERFORMANCE_TUNING.md) covers upload, FFmpeg, Autonomi,
  and stream-cache settings.
- [Image publishing](IMAGE_PUBLISHING.md) covers GHCR image build and tag
  workflows.
