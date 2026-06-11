# Desktop App

The installable desktop app lives in `desktop_app/` and uses Tauri v2 as a
native shell around the existing local service stack.

## Runtime Shape

The desktop app keeps the existing service boundaries:

- `antd`
- `rust_admin`
- `rust_stream`
- bundled `ffmpeg` and `ffprobe`
- React frontend served through the local launcher proxy

On first run, the Tauri shell requires admin credentials and an Autonomi wallet
key or key-file path. The launcher writes file-backed secrets into the app data
directory with private file permissions and starts services with
`AUTVID_STRICT_AUTH=true`, `APP_ENV=production`, and file-backed admin secrets.

Local devnet mode remains a developer workflow through Compose or the
standalone launcher; normal desktop builds use the configured Autonomi network.

## Build Locally

```bash
make install-react
make install-desktop
make build-react
make stage-tauri-sidecars
make build-tauri
```

`make stage-tauri-sidecars` builds the Rust sidecars and copies `ffmpeg` and
`ffprobe` from `FFMPEG_BIN` / `FFPROBE_BIN` or from `PATH` into
`desktop_app/src-tauri/binaries` using Tauri's target-triple sidecar naming.

## Release Notes

The `Desktop Release` GitHub Actions workflow builds Linux AppImage/deb/rpm
artifacts and macOS app/dmg artifacts. macOS public beta releases require
Developer ID signing and notarization secrets:

- `APPLE_CERTIFICATE`
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`
- `APPLE_PASSWORD`
- `APPLE_TEAM_ID`

Do not publish unsigned macOS artifacts as a public beta.
