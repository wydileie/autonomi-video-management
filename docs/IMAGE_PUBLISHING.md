# Image Publishing

The `Publish Images` workflow builds and pushes the runtime Docker images to
GitHub Container Registry (GHCR). It publishes images for the existing service
Dockerfiles owned by the Compose stack:

| Service image | Dockerfile | Published image |
|---|---|---|
| Rust admin API | `rust_admin/Dockerfile` | `ghcr.io/OWNER/autonomi-video-management-rust-admin` |
| Rust stream API | `rust_stream/Dockerfile` | `ghcr.io/OWNER/autonomi-video-management-rust-stream` |
| Production `antd` gateway | `antd_service/Dockerfile` | `ghcr.io/OWNER/autonomi-video-management-antd-service` |
| Nginx reverse proxy | `nginx/Dockerfile` | `ghcr.io/OWNER/autonomi-video-management-nginx` |
| React frontend | `react_frontend/Dockerfile` | `ghcr.io/OWNER/autonomi-video-management-react-frontend` |
| Postgres with init scripts | `postgres-init/Dockerfile` | `ghcr.io/OWNER/autonomi-video-management-postgres-init` |
| Local Autonomi devnet | `autonomi_devnet/Dockerfile` | `ghcr.io/OWNER/autonomi-video-management-autonomi-devnet` |

Replace `OWNER` with the lower-case GitHub user or organization that owns the
repository. The workflow lower-cases `OWNER/autonomi-video-management` before
pushing because GHCR image names must be lower-case.

## Triggers

The workflow runs on:

| Event | Result |
|---|---|
| Push to `main` | Builds all images, publishes `main`, `sha-<short-sha>`, and `latest` tags |
| Push of a semver-style tag such as `v1.2.3` or `1.2.3` | Builds all images, publishes semver tags such as `1.2.3` and `1.2`, plus `sha-<short-sha>` and `latest` |
| Manual `workflow_dispatch` | Builds all images for the selected ref; tags follow the branch or tag selected |

The `latest` tag is only produced for the default branch or a semver release
tag. Prefer immutable semver or `sha-<short-sha>` tags for production rollouts.

## Permissions

The workflow uses the repository `GITHUB_TOKEN` and declares:

```yaml
permissions:
  contents: read
  packages: write
```

No personal access token is required when publishing packages associated with
this repository. If the organization disables automatic package inheritance, or
if a package with the same name was created manually before this workflow, grant
this repository Actions access to the package in the package settings.

GHCR packages are private on first publish unless repository or organization
defaults make them public. For private packages, users pulling locally need to
authenticate with an account that has package read access:

```bash
echo "$GITHUB_TOKEN_OR_PAT" | docker login ghcr.io -u USERNAME --password-stdin
```

Use a classic personal access token with `read:packages` for local pulls from
private GHCR packages. Public GHCR packages can be pulled without login.

## Pull Examples

```bash
OWNER=your-org
TAG=1.2.3

docker pull ghcr.io/${OWNER}/autonomi-video-management-rust-admin:${TAG}
docker pull ghcr.io/${OWNER}/autonomi-video-management-rust-stream:${TAG}
docker pull ghcr.io/${OWNER}/autonomi-video-management-antd-service:${TAG}
docker pull ghcr.io/${OWNER}/autonomi-video-management-nginx:${TAG}
docker pull ghcr.io/${OWNER}/autonomi-video-management-react-frontend:${TAG}
docker pull ghcr.io/${OWNER}/autonomi-video-management-postgres-init:${TAG}
docker pull ghcr.io/${OWNER}/autonomi-video-management-autonomi-devnet:${TAG}
```

For a commit-pinned image, use the workflow's SHA tag:

```bash
docker pull ghcr.io/${OWNER}/autonomi-video-management-rust-admin:sha-abc1234
```

## Compose Overrides

The checked-in Compose files keep local `build:` entries so development stays
self-contained. To run with published images, create an uncommitted override
that replaces selected `build:` entries with GHCR images.

Common base override:

```yaml
# docker-compose.images.yml
services:
  db:
    image: ${AUTVID_IMAGE_PREFIX}-postgres-init:${AUTVID_IMAGE_TAG:-latest}
    build: !reset null

  rust_admin:
    image: ${AUTVID_IMAGE_PREFIX}-rust-admin:${AUTVID_IMAGE_TAG:-latest}
    build: !reset null

  rust_stream:
    image: ${AUTVID_IMAGE_PREFIX}-rust-stream:${AUTVID_IMAGE_TAG:-latest}
    build: !reset null

  react_frontend:
    image: ${AUTVID_IMAGE_PREFIX}-react-frontend:${AUTVID_IMAGE_TAG:-latest}
    build: !reset null

  nginx:
    image: ${AUTVID_IMAGE_PREFIX}-nginx:${AUTVID_IMAGE_TAG:-latest}
    build: !reset null
```

Production `antd` override:

```yaml
# docker-compose.prod.images.yml
services:
  antd:
    image: ${AUTVID_IMAGE_PREFIX}-antd-service:${AUTVID_IMAGE_TAG:-latest}
    build: !reset null
```

Local-devnet `antd` override:

```yaml
# docker-compose.local.images.yml
services:
  antd:
    image: ${AUTVID_IMAGE_PREFIX}-autonomi-devnet:${AUTVID_IMAGE_TAG:-latest}
    build: !reset null
```

Run production with published backend/proxy images:

```bash
export AUTVID_IMAGE_PREFIX=ghcr.io/OWNER/autonomi-video-management
export AUTVID_IMAGE_TAG=1.2.3

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.images.yml \
  -f docker-compose.prod.images.yml \
  up -d
```

Run local devnet with published backend/proxy/devnet images:

```bash
export AUTVID_IMAGE_PREFIX=ghcr.io/OWNER/autonomi-video-management
export AUTVID_IMAGE_TAG=sha-abc1234

docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.images.yml \
  -f docker-compose.local.images.yml \
  up -d
```

The frontend image is built with the default runtime config paths used by
Compose: `/api` for the admin API and `/stream` for playback.

## References

- [GitHub Container registry documentation](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry)
- [Docker GitHub Actions guide](https://docs.docker.com/guides/gha/)
- [Docker tag and label automation guide](https://docs.docker.com/build/ci/github-actions/manage-tags-labels/)
