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
| `REACT_APP_API_URL` | `/api` | Base URL for the Rust admin API |
| `REACT_APP_STREAM_URL` | `/stream` | Base URL for the Rust streaming service |

## Runtime browser configuration

The app also loads `/runtime-config.js` before the React bundle. Container builds ship a harmless default that leaves the existing `/api` and `/stream` behavior unchanged, while a native host or deployment wrapper can replace the file or define `window.__AUTONOMI_VIDEO_CONFIG__` first:

```js
window.__AUTONOMI_VIDEO_CONFIG__ = {
  apiBaseUrl: "http://localhost:8000/api",
  streamBaseUrl: "http://localhost:8081/stream",
};
```

Runtime browser config wins over build-time env values. If neither is provided, the app keeps using `/api` and `/stream`.

## Local development

```bash
cd react_frontend
npm install
REACT_APP_API_URL=http://localhost/api \
REACT_APP_STREAM_URL=http://localhost/stream \
npm start
# Opens http://localhost:5173
```

The local stack should already be running behind Nginx:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  up --build
```

If you specifically need direct service ports for debugging, include
`docker-compose.debug-ports.yml` and use `http://localhost:8000` and
`http://localhost:8081`.

## Build

```bash
npm run build
# Output: build/
```

The production Dockerfile uses a multi-stage build: Node 24 Bookworm Slim builds the static assets with Vite, then copies them into an `nginx:1.27-alpine` image with a simple SPA fallback config (all routes -> `index.html`).

## Dependencies

| Package | Purpose |
|---|---|
| `react` / `react-dom` | UI framework |
| `hls.js` | HLS adaptive streaming player |
| `axios` | HTTP requests to the admin API |
| `vite` / `vitest` | Build tooling and unit test runner |

## Project structure

```
index.html     # Vite HTML shell
vite.config.mjs
public/
└── runtime-config.js # Optional runtime browser config hook
src/
├── main.jsx
└── App.jsx     # All components in one file: App, UploadPanel, Library, VideoPlayer
```

`App.jsx` contains four components:

| Component | Description |
|---|---|
| `App` | Root: tab state (Library / Upload), nav bar |
| `UploadPanel` | File input, title/description fields, resolution checkboxes, upload progress |
| `Library` | Video list table, status polling, row expand, resolution selector |
| `VideoPlayer` | hls.js wrapper; attaches/detaches on mount/unmount |
