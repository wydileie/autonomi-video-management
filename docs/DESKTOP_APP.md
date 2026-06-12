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
`AUTVID_STRICT_AUTH=true`, `APP_ENV=production`, `ADMIN_AUTH_COOKIE_SECURE=false`,
and file-backed admin secrets. Desktop uses non-Secure auth cookies because the
installed app talks to the local launcher over loopback HTTP; hosted HTTPS
deployments should keep Secure cookies enabled.
If the wallet key is pasted into first-run setup, it is stored in the app data
directory under `secrets/autonomi-wallet-key` with private file permissions. If
you select an existing wallet key file instead, the app stores only that file
path.

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
`ffprobe` into `desktop_app/src-tauri/binaries` using Tauri's target-triple
sidecar naming. Release builds must provide self-contained FFmpeg tools with
either:

```bash
FFMPEG_BIN=/path/to/ffmpeg FFPROBE_BIN=/path/to/ffprobe make stage-tauri-sidecars
```

or:

```bash
FFMPEG_DIST_DIR=/path/to/media-tools make stage-tauri-sidecars
```

where `FFMPEG_DIST_DIR` contains executable `ffmpeg` and `ffprobe` files.

For local developer builds only, you can opt into copying tools from `PATH`:

```bash
AUTVID_ALLOW_SYSTEM_FFMPEG=1 AUTVID_ALLOW_DYNAMIC_FFMPEG=1 make stage-tauri-sidecars
```

Do not use Homebrew, MacPorts, or apt-provided dynamic FFmpeg binaries for
public desktop artifacts unless the release bundle also includes and signs their
non-system shared libraries.

## Release Notes

The `Desktop Release` GitHub Actions workflow builds Linux AppImage/deb/rpm
artifacts and macOS app/dmg artifacts. The workflow expects platform-specific
repository variables that point to `tar.gz` archives containing self-contained
`ffmpeg` and `ffprobe` binaries at the archive root:

- `DESKTOP_MEDIA_TOOLS_LINUX_URL`
- `DESKTOP_MEDIA_TOOLS_MACOS_URL`

macOS public beta releases require Developer ID signing and notarization
secrets:

- `APPLE_CERTIFICATE`
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`
- `APPLE_PASSWORD`
- `APPLE_TEAM_ID`

Do not publish unsigned macOS artifacts as a public beta.
