# React Frontend

Single-page React application for uploading videos, monitoring processing status, and playing back stored content via HLS.

## Features

- **Upload panel** — drag/drop or select a video file, inspect its source resolution in-browser, pick one or more tiers (8K / 4K / 1080P / 720p / 480P / 360P), and submit. Shows a real upload progress bar and then a "processing" indicator while FFmpeg + Autonomi upload runs server-side.
- **Video library** — lists all videos with status badges (`pending`, `processing`, `ready`, `error`). Auto-polls every 5 seconds while any video is still processing.
- **Inline player** — click a video row to expand it, choose a resolution, and play directly in the browser using [hls.js](https://github.com/video-dev/hls.js). The player streams from the Rust streaming service which proxies segments from the Autonomi network.

## Environment variables (build-time)

Set via Docker Compose `build.args` in `docker-compose.yml` — these are baked into the static build at image build time.

| Variable | Default | Description |
|---|---|---|
| `REACT_APP_API_URL` | `/api` | Base URL for the Python admin API |
| `REACT_APP_STREAM_URL` | `/stream` | Base URL for the Rust streaming service |

## Local development

```bash
cd react_frontend
npm install
REACT_APP_API_URL=http://localhost:8000 \
REACT_APP_STREAM_URL=http://localhost:8081 \
npm start
# Opens http://localhost:3000
```

The admin API and streaming service must already be running. For the full local
stack, use:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  up --build
```

## Build

```bash
npm run build
# Output: build/
```

The production Dockerfile uses a multi-stage build: Node 18 Alpine builds the static assets, then copies them into an `nginx:alpine` image with a simple SPA fallback config (all routes → `index.html`).

## Dependencies

| Package | Purpose |
|---|---|
| `react` / `react-dom` | UI framework |
| `hls.js` | HLS adaptive streaming player |
| `axios` | HTTP requests to the admin API |
| `react-scripts` | CRA build tooling |

## Project structure

```
src/
└── App.js      # All components in one file: App, UploadPanel, Library, VideoPlayer
public/
└── index.html  # HTML shell
```

`App.js` contains four components:

| Component | Description |
|---|---|
| `App` | Root: tab state (Library / Upload), nav bar |
| `UploadPanel` | File input, title/description fields, resolution checkboxes, upload progress |
| `Library` | Video list table, status polling, row expand, resolution selector |
| `VideoPlayer` | hls.js wrapper; attaches/detaches on mount/unmount |
