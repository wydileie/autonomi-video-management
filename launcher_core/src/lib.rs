use std::{
    env,
    fs::{self, File, OpenOptions},
    io::Write,
    net::TcpListener as StdTcpListener,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{anyhow, Context};
use axum::{
    body::Body,
    extract::{Path as AxumPath, State},
    http::{header, HeaderMap, Request, Response, StatusCode},
    response::IntoResponse,
    routing::{any, get},
    Router,
};
use fs2::FileExt;
use rand::{rngs::OsRng, RngCore};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    process::{Child, Command},
    task::JoinHandle,
    time::{sleep, Instant},
};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};

const CONFIG_FILE: &str = "desktop-config.json";
const PASSWORD_FILE: &str = "admin-password";
const AUTH_SECRET_FILE: &str = "admin-auth-secret";
const WALLET_KEY_FILE: &str = "autonomi-wallet-key";
const MANAGED_CHILD_NAMES: &[&str] = &["antd", "local-devnet", "rust_admin", "rust_stream"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkMode {
    Configured,
    LocalDevnet,
}

#[derive(Clone, Debug)]
pub struct LaunchOptions {
    pub mode: NetworkMode,
    pub app_name: String,
    pub data_dir: Option<PathBuf>,
    pub binary_dir: Option<PathBuf>,
    pub frontend_dir: Option<PathBuf>,
    pub require_setup: bool,
    pub open_browser: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            mode: NetworkMode::Configured,
            app_name: "Autonomi Video Management".to_string(),
            data_dir: None,
            binary_dir: None,
            frontend_dir: None,
            require_setup: false,
            open_browser: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct FirstRunSetup {
    pub admin_username: String,
    pub admin_password: String,
    pub wallet_key: Option<String>,
    pub wallet_key_file: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetupStatus {
    pub configured: bool,
    pub data_dir: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DesktopConfig {
    schema_version: u8,
    admin_username: String,
    admin_password_file: PathBuf,
    admin_auth_secret_file: PathBuf,
    wallet_key_file: Option<PathBuf>,
}

#[derive(Debug)]
pub struct RunningStack {
    pub url: String,
    _lock_file: File,
    children: Vec<ManagedChild>,
    server: JoinHandle<anyhow::Result<()>>,
}

#[derive(Debug)]
struct ManagedChild {
    name: String,
    child: Child,
    pid_file: PathBuf,
}

#[derive(Clone)]
struct ProxyState {
    admin_base: String,
    client: Client,
    runtime_config_js: String,
    stream_base: String,
}

impl RunningStack {
    pub async fn wait(mut self) -> anyhow::Result<()> {
        let result = self.server.await.context("launcher server task failed")?;
        stop_children(&mut self.children).await;
        result
    }

    pub async fn shutdown(mut self) {
        self.server.abort();
        let _ = self.server.await;
        stop_children(&mut self.children).await;
    }
}

pub fn resolve_data_dir(app_name: &str) -> anyhow::Result<PathBuf> {
    if let Ok(value) = env::var("AUTVID_DATA_DIR") {
        return Ok(PathBuf::from(value));
    }
    let home = env::var("HOME").context("HOME is required when AUTVID_DATA_DIR is unset")?;
    if cfg!(target_os = "macos") {
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join(app_name))
    } else {
        let linux_name = app_name
            .to_ascii_lowercase()
            .replace(' ', "-")
            .replace("--", "-");
        Ok(env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(home).join(".local").join("share"))
            .join(linux_name))
    }
}

pub fn setup_status(data_dir: &Path) -> SetupStatus {
    SetupStatus {
        configured: read_desktop_config(data_dir).is_ok(),
        data_dir: data_dir.to_string_lossy().to_string(),
    }
}

pub fn save_first_run_config(data_dir: &Path, setup: FirstRunSetup) -> anyhow::Result<SetupStatus> {
    validate_setup(&setup)?;
    fs::create_dir_all(data_dir)?;
    let secrets_dir = data_dir.join("secrets");
    fs::create_dir_all(&secrets_dir)?;
    set_private_dir_permissions(&secrets_dir)?;

    let password_file = secrets_dir.join(PASSWORD_FILE);
    write_secret_file(&password_file, setup.admin_password.as_bytes())?;
    let auth_secret_file = secrets_dir.join(AUTH_SECRET_FILE);
    write_secret_file(&auth_secret_file, random_hex_secret().as_bytes())?;

    let wallet_key_file = match (
        setup.wallet_key.map(|value| value.trim().to_string()),
        setup.wallet_key_file.map(|value| value.trim().to_string()),
    ) {
        (Some(key), _) if !key.is_empty() => {
            let path = secrets_dir.join(WALLET_KEY_FILE);
            write_secret_file(&path, key.as_bytes())?;
            Some(path)
        }
        (_, Some(path)) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => None,
    };

    let config = DesktopConfig {
        schema_version: 1,
        admin_username: setup.admin_username.trim().to_string(),
        admin_password_file: password_file,
        admin_auth_secret_file: auth_secret_file,
        wallet_key_file,
    };
    let encoded = serde_json::to_vec_pretty(&config)?;
    write_private_file(&data_dir.join(CONFIG_FILE), &encoded)?;
    Ok(setup_status(data_dir))
}

pub async fn launch_stack(options: LaunchOptions) -> anyhow::Result<RunningStack> {
    let data_dir = options
        .data_dir
        .clone()
        .unwrap_or(resolve_data_dir(&options.app_name)?);
    if options.require_setup && read_desktop_config(&data_dir).is_err() {
        return Err(anyhow!("desktop first-run setup has not been completed"));
    }

    let processing_dir = data_dir.join("processing");
    let catalog_dir = data_dir.join("catalog");
    let logs_dir = data_dir.join("logs");
    let run_dir = data_dir.join("run");
    tokio::fs::create_dir_all(&processing_dir).await?;
    tokio::fs::create_dir_all(&catalog_dir).await?;
    tokio::fs::create_dir_all(&logs_dir).await?;
    tokio::fs::create_dir_all(&run_dir).await?;
    set_private_dir_permissions(&run_dir)?;
    let lock_file = acquire_instance_lock(&run_dir)?;
    cleanup_stale_children(&run_dir)?;

    let admin_port = env_port_or_available("RUST_ADMIN_PORT", 8000)?;
    let stream_port = env_port_or_available("RUST_STREAM_PORT", 8081)?;
    let antd_port = env_port_or_available("ANTD_REST_PORT", 8082)?;
    let launcher_port = env_port_or_available("AUTVID_LAUNCHER_PORT", 8080)?;

    let antd_url = format!("http://127.0.0.1:{antd_port}");
    let admin_base = format!("http://127.0.0.1:{admin_port}");
    let stream_base = format!("http://127.0.0.1:{stream_port}");
    let launcher_url = format!("http://127.0.0.1:{launcher_port}");
    let desktop_config = read_desktop_config(&data_dir).ok();
    let tool_paths = ToolPaths::resolve(options.binary_dir.as_deref());

    let mut children = Vec::new();
    macro_rules! cleanup_try {
        ($expr:expr) => {
            match $expr {
                Ok(value) => value,
                Err(err) => {
                    stop_children(&mut children).await;
                    return Err(err.into());
                }
            }
        };
    }

    children.push(start_antd(
        options.mode,
        &data_dir,
        &logs_dir,
        &run_dir,
        antd_port,
        options.binary_dir.as_deref(),
        desktop_config.as_ref(),
    )?);
    cleanup_try!(wait_for_health(&format!("{antd_url}/livez"), "antd").await);

    children.push(cleanup_try!(start_rust_admin(
        &data_dir,
        &processing_dir,
        &catalog_dir,
        &logs_dir,
        &run_dir,
        admin_port,
        &antd_url,
        &launcher_url,
        options.binary_dir.as_deref(),
        desktop_config.as_ref(),
        &tool_paths,
    )));
    children.push(cleanup_try!(start_rust_stream(
        stream_port,
        &antd_url,
        &catalog_dir,
        &logs_dir,
        &run_dir,
        &launcher_url,
        options.binary_dir.as_deref(),
    )));

    cleanup_try!(wait_for_health(&format!("{admin_base}/livez"), "rust_admin").await);
    cleanup_try!(wait_for_health(&format!("{stream_base}/livez"), "rust_stream").await);

    let frontend_dir = cleanup_try!(resolve_frontend_dir(options.frontend_dir.as_deref()));
    let state = ProxyState {
        admin_base,
        client: Client::new(),
        runtime_config_js:
            "window.__AUTONOMI_VIDEO_CONFIG__ = { apiBaseUrl: '/api', streamBaseUrl: '/stream' };\n"
                .to_string(),
        stream_base,
    };
    let app = Router::new()
        .route("/runtime-config.js", get(runtime_config))
        .route("/api/{*path}", any(proxy_admin))
        .route("/stream/{*path}", any(proxy_stream))
        .fallback_service(
            ServeDir::new(&frontend_dir)
                .not_found_service(ServeFile::new(frontend_dir.join("index.html"))),
        )
        .with_state(state);

    let listener = cleanup_try!(TcpListener::bind(("127.0.0.1", launcher_port)).await);
    info!("AutVid launcher ready at {}", launcher_url);
    if options.open_browser {
        open_browser(&launcher_url);
    }

    let server = tokio::spawn(async move {
        axum::serve(listener, app).await?;
        Ok(())
    });
    Ok(RunningStack {
        url: launcher_url,
        _lock_file: lock_file,
        children,
        server,
    })
}

fn validate_setup(setup: &FirstRunSetup) -> anyhow::Result<()> {
    let username = setup.admin_username.trim();
    if username.is_empty() || is_unsafe_admin_auth_value(username) {
        return Err(anyhow!("choose a non-default admin username"));
    }
    let password = setup.admin_password.trim();
    if password.len() < 12 || is_unsafe_admin_auth_value(password) {
        return Err(anyhow!(
            "admin password must be at least 12 characters and non-default"
        ));
    }
    if setup
        .wallet_key
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
        && setup
            .wallet_key_file
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(anyhow!(
            "provide an Autonomi wallet key or wallet key file path"
        ));
    }
    if let Some(path) = setup
        .wallet_key_file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(anyhow!(
                "Autonomi wallet key file does not exist or is not a file"
            ));
        }
    }
    Ok(())
}

fn random_hex_secret() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_desktop_config(data_dir: &Path) -> anyhow::Result<DesktopConfig> {
    let config_path = data_dir.join(CONFIG_FILE);
    let raw = fs::read(&config_path).with_context(|| {
        format!(
            "could not read desktop config at {}",
            config_path.to_string_lossy()
        )
    })?;
    let config: DesktopConfig = serde_json::from_slice(&raw)?;
    if config.schema_version != 1 {
        return Err(anyhow!("unsupported desktop config schema"));
    }
    if !config.admin_password_file.is_file() || !config.admin_auth_secret_file.is_file() {
        return Err(anyhow!("desktop setup secret files are missing"));
    }
    Ok(config)
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut contents = bytes.to_vec();
    contents.push(b'\n');
    write_private_file(path, &contents)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    set_private_file_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn start_antd(
    mode: NetworkMode,
    data_dir: &Path,
    logs_dir: &Path,
    run_dir: &Path,
    antd_port: u16,
    binary_dir: Option<&Path>,
    desktop_config: Option<&DesktopConfig>,
) -> anyhow::Result<ManagedChild> {
    match mode {
        NetworkMode::Configured => {
            let mut command = child_command("AUTVID_ANTD_BIN", "antd", binary_dir);
            command
                .env("ANTD_REST_ADDR", format!("127.0.0.1:{antd_port}"))
                .env("ANTD_UPLOAD_TEMP_DIR", data_dir.join("antd-temp"));
            if let Some(wallet_key_file) =
                desktop_config.and_then(|config| config.wallet_key_file.as_ref())
            {
                command.env("AUTONOMI_WALLET_KEY_FILE", wallet_key_file);
            }
            spawn("antd", command, logs_dir, run_dir)
        }
        NetworkMode::LocalDevnet => {
            let script = env::var("AUTVID_DEVNET_CMD")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../autonomi_devnet/start-local-devnet.sh")
                });
            let mut command = Command::new(script);
            command
                .env("ANTD_REST_ADDR", format!("127.0.0.1:{antd_port}"))
                .env("ANT_DEVNET_DATA_DIR", data_dir.join("devnet").join("nodes"))
                .env("LOG_DIR", data_dir.join("devnet").join("logs"));
            spawn("local-devnet", command, logs_dir, run_dir)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn start_rust_admin(
    data_dir: &Path,
    processing_dir: &Path,
    catalog_dir: &Path,
    logs_dir: &Path,
    run_dir: &Path,
    admin_port: u16,
    antd_url: &str,
    launcher_url: &str,
    binary_dir: Option<&Path>,
    desktop_config: Option<&DesktopConfig>,
    tool_paths: &ToolPaths,
) -> anyhow::Result<ManagedChild> {
    let mut command = child_command("AUTVID_ADMIN_BIN", "rust_admin", binary_dir);
    command
        .env("AUTVID_DATA_DIR", data_dir)
        .env("ADMIN_DB_PATH", data_dir.join("autvid.sqlite3"))
        .env("CATALOG_STATE_PATH", catalog_dir.join("catalog.json"))
        .env("UPLOAD_TEMP_DIR", processing_dir)
        .env("TMPDIR", processing_dir)
        .env("TMP", processing_dir)
        .env("TEMP", processing_dir)
        .env("ANTD_URL", antd_url)
        .env("RUST_ADMIN_PORT", admin_port.to_string())
        .env("CORS_ALLOWED_ORIGINS", launcher_url);
    // The desktop launcher serves the production-built app over loopback HTTP.
    // Keep strict auth enabled, but do not mark cookies Secure unless the
    // browser is actually talking to the admin service over HTTPS.
    apply_desktop_admin_auth_env(&mut command);
    command
        .env(
            "ADMIN_DB_MIN_CONNECTIONS",
            env::var("ADMIN_DB_MIN_CONNECTIONS").unwrap_or_else(|_| "1".into()),
        )
        .env(
            "ADMIN_DB_MAX_CONNECTIONS",
            env::var("ADMIN_DB_MAX_CONNECTIONS").unwrap_or_else(|_| "5".into()),
        );
    if let Some(config) = desktop_config {
        command
            .env("ADMIN_USERNAME", &config.admin_username)
            .env("ADMIN_PASSWORD_FILE", &config.admin_password_file)
            .env("ADMIN_AUTH_SECRET_FILE", &config.admin_auth_secret_file);
    }
    if let Some(ffmpeg) = &tool_paths.ffmpeg {
        command.env("FFMPEG_BIN", ffmpeg);
    }
    if let Some(ffprobe) = &tool_paths.ffprobe {
        command.env("FFPROBE_BIN", ffprobe);
    }
    spawn("rust_admin", command, logs_dir, run_dir)
}

fn desktop_admin_auth_env() -> &'static [(&'static str, &'static str)] {
    &[
        ("APP_ENV", "production"),
        ("ADMIN_AUTH_COOKIE_SECURE", "false"),
        ("AUTVID_STRICT_AUTH", "true"),
    ]
}

fn apply_desktop_admin_auth_env(command: &mut Command) {
    for (key, value) in desktop_admin_auth_env() {
        command.env(key, value);
    }
}

fn start_rust_stream(
    stream_port: u16,
    antd_url: &str,
    catalog_dir: &Path,
    logs_dir: &Path,
    run_dir: &Path,
    launcher_url: &str,
    binary_dir: Option<&Path>,
) -> anyhow::Result<ManagedChild> {
    let mut command = child_command("AUTVID_STREAM_BIN", "rust_stream", binary_dir);
    command
        .env("ANTD_URL", antd_url)
        .env("CATALOG_STATE_PATH", catalog_dir.join("catalog.json"))
        .env("RUST_STREAM_PORT", stream_port.to_string())
        .env("CORS_ALLOWED_ORIGINS", launcher_url);
    spawn("rust_stream", command, logs_dir, run_dir)
}

fn child_command(env_name: &str, fallback: &str, binary_dir: Option<&Path>) -> Command {
    if let Ok(path) = env::var(env_name) {
        return Command::new(path);
    }
    if let Some(dir) = binary_dir {
        if let Some(candidate) = binary_candidate(dir, fallback) {
            return Command::new(candidate);
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(fallback);
            if sibling.is_file() {
                return Command::new(sibling);
            }
        }
    }
    Command::new(fallback)
}

fn spawn(
    name: &str,
    mut command: Command,
    logs_dir: &Path,
    run_dir: &Path,
) -> anyhow::Result<ManagedChild> {
    info!("Starting {}", name);
    let stdout = append_log(logs_dir, &format!("{name}.log"))?;
    let stderr = append_log(logs_dir, &format!("{name}.err.log"))?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {name}"))?;
    let Some(pid) = child.id() else {
        let _ = child.start_kill();
        return Err(anyhow!("{name} did not report a process id"));
    };
    let pid_file = run_dir.join(format!("{name}.pid"));
    if let Err(err) = write_private_file(&pid_file, pid.to_string().as_bytes()) {
        let _ = child.start_kill();
        return Err(err).with_context(|| format!("could not write {name} pid file"));
    }
    Ok(ManagedChild {
        name: name.to_string(),
        child,
        pid_file,
    })
}

fn append_log(logs_dir: &Path, file_name: &str) -> anyhow::Result<std::fs::File> {
    fs::create_dir_all(logs_dir)?;
    Ok(OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs_dir.join(file_name))?)
}

fn acquire_instance_lock(run_dir: &Path) -> anyhow::Result<File> {
    let lock_path = run_dir.join("launcher.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("could not open lock file {}", lock_path.to_string_lossy()))?;
    match lock_file.try_lock_exclusive() {
        Ok(()) => Ok(lock_file),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Err(anyhow!(
            "another Autonomi Video Management instance is already using {}",
            run_dir.parent().unwrap_or(run_dir).to_string_lossy()
        )),
        Err(err) => Err(err).with_context(|| {
            format!(
                "could not lock launcher instance file {}",
                lock_path.to_string_lossy()
            )
        }),
    }
}

async fn wait_for_health(url: &str, name: &str) -> anyhow::Result<()> {
    let client = Client::new();
    for _ in 0..120 {
        if client
            .get(url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            info!("{} is healthy", name);
            return Ok(());
        }
        sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("{name} did not become healthy at {url}"))
}

fn env_port_or_available(name: &str, default: u16) -> anyhow::Result<u16> {
    if let Ok(value) = env::var(name) {
        return value
            .parse::<u16>()
            .with_context(|| format!("{name} must be a valid TCP port"));
    }
    if port_available(default) {
        return Ok(default);
    }
    let listener = StdTcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn port_available(port: u16) -> bool {
    StdTcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn env_duration(name: &str, default: Duration) -> Duration {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(default)
}

fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(err) = std::process::Command::new(opener).arg(url).spawn() {
        warn!("Could not open browser with {}: {}", opener, err);
    }
}

async fn stop_children(children: &mut [ManagedChild]) {
    let shutdown_grace = env_duration("ADMIN_SHUTDOWN_GRACE_SECONDS", Duration::from_secs(15));
    for managed in children.iter_mut() {
        if let Err(err) = terminate_child(&mut managed.child) {
            warn!("Could not signal {} shutdown: {}", managed.name, err);
        }
    }

    let deadline = Instant::now() + shutdown_grace;
    let mut running = vec![true; children.len()];
    while running.iter().any(|is_running| *is_running) && Instant::now() < deadline {
        for (index, managed) in children.iter_mut().enumerate() {
            if !running[index] {
                continue;
            }
            match managed.child.try_wait() {
                Ok(Some(_status)) => running[index] = false,
                Ok(None) => {}
                Err(err) => {
                    warn!("Could not check {} shutdown status: {}", managed.name, err);
                    running[index] = false;
                }
            }
        }
        if running.iter().any(|is_running| *is_running) {
            sleep(Duration::from_millis(100)).await;
        }
    }

    for managed in children.iter_mut() {
        match managed.child.try_wait() {
            Ok(Some(_status)) => {}
            Ok(None) => {
                warn!(
                    "{} did not stop within {:?}; killing it",
                    managed.name, shutdown_grace
                );
                if let Err(err) = managed.child.start_kill() {
                    warn!("Could not kill {} process: {}", managed.name, err);
                }
            }
            Err(err) => warn!(
                "Could not check {} status before kill: {}",
                managed.name, err
            ),
        }
        let _ = managed.child.wait().await;
        if let Err(err) = fs::remove_file(&managed.pid_file) {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    "Could not remove {} pid file {}: {}",
                    managed.name,
                    managed.pid_file.to_string_lossy(),
                    err
                );
            }
        }
    }
}

fn cleanup_stale_children(run_dir: &Path) -> anyhow::Result<()> {
    for name in MANAGED_CHILD_NAMES {
        let pid_file = run_dir.join(format!("{name}.pid"));
        let raw = match fs::read_to_string(&pid_file) {
            Ok(value) => value,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                warn!(
                    "Could not read stale {} pid file {}: {}",
                    name,
                    pid_file.to_string_lossy(),
                    err
                );
                continue;
            }
        };
        let Ok(pid) = raw.trim().parse::<u32>() else {
            warn!(
                "Removing invalid stale {} pid file {}",
                name,
                pid_file.to_string_lossy()
            );
            let _ = fs::remove_file(&pid_file);
            continue;
        };
        if stale_child_running(pid, name) {
            warn!("Stopping stale {} sidecar with pid {}", name, pid);
            if let Err(err) = terminate_pid(pid) {
                warn!("Could not stop stale {} sidecar {}: {}", name, pid, err);
            }
        }
        let _ = fs::remove_file(&pid_file);
    }
    Ok(())
}

#[cfg(unix)]
fn terminate_child(child: &mut Child) -> std::io::Result<()> {
    let Some(pid) = child.id() else {
        return Ok(());
    };
    terminate_pid(pid)
}

#[cfg(unix)]
fn terminate_pid(pid: u32) -> std::io::Result<()> {
    let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn terminate_child(child: &mut Child) -> std::io::Result<()> {
    child.start_kill()
}

#[cfg(not(unix))]
fn terminate_pid(_pid: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stale pid cleanup is not implemented on this platform",
    ))
}

#[cfg(unix)]
fn stale_child_running(pid: u32, expected_name: &str) -> bool {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result != 0 {
        return false;
    }
    process_name_matches(pid, expected_name).unwrap_or(false)
}

#[cfg(not(unix))]
fn stale_child_running(_pid: u32, _expected_name: &str) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn process_name_matches(pid: u32, expected_name: &str) -> Option<bool> {
    let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(process_name_is_expected(comm.trim(), expected_name))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_name_matches(pid: u32, expected_name: &str) -> Option<bool> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        return Some(false);
    }
    let command = String::from_utf8_lossy(&output.stdout);
    Some(
        Path::new(command.trim())
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|name| process_name_is_expected(name, expected_name)),
    )
}

#[cfg(unix)]
fn process_name_is_expected(candidate: &str, expected_name: &str) -> bool {
    candidate == expected_name
        || candidate
            .strip_prefix(expected_name)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

fn resolve_frontend_dir(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(value) = explicit {
        return Ok(value.to_path_buf());
    }
    if let Ok(value) = env::var("AUTVID_FRONTEND_DIR") {
        return Ok(PathBuf::from(value));
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_build = manifest_dir.join("../react_frontend/build");
    if repo_build.join("index.html").is_file() {
        return Ok(repo_build);
    }
    let exe_dir = env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("could not resolve launcher executable directory"))?;
    for candidate in [
        exe_dir.join("frontend"),
        exe_dir.join("../Resources/frontend"),
    ] {
        if candidate.join("index.html").is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "frontend build not found; set AUTVID_FRONTEND_DIR to a directory containing index.html"
    ))
}

#[derive(Debug)]
struct ToolPaths {
    ffmpeg: Option<PathBuf>,
    ffprobe: Option<PathBuf>,
}

impl ToolPaths {
    fn resolve(binary_dir: Option<&Path>) -> Self {
        Self {
            ffmpeg: resolve_tool("FFMPEG_BIN", "ffmpeg", binary_dir),
            ffprobe: resolve_tool("FFPROBE_BIN", "ffprobe", binary_dir),
        }
    }
}

fn resolve_tool(env_name: &str, fallback: &str, binary_dir: Option<&Path>) -> Option<PathBuf> {
    if let Ok(path) = env::var(env_name) {
        return Some(PathBuf::from(path));
    }
    binary_dir.and_then(|dir| binary_candidate(dir, fallback))
}

fn binary_candidate(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    let prefix = format!("{name}-");
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .is_some_and(|file_name| file_name.starts_with(&prefix))
        })
}

async fn runtime_config(State(state): State<ProxyState>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        state.runtime_config_js,
    )
}

async fn proxy_admin(
    State(state): State<ProxyState>,
    AxumPath(path): AxumPath<String>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response<Body> {
    proxy_to(state, headers, request, format!("/{}", path), true).await
}

async fn proxy_stream(
    State(state): State<ProxyState>,
    AxumPath(path): AxumPath<String>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response<Body> {
    proxy_to(state, headers, request, stream_proxy_path(&path), false).await
}

fn stream_proxy_path(path: &str) -> String {
    // rust_stream mounts playback routes under /stream; preserve the prefix Axum strips here.
    format!("/stream/{path}")
}

async fn proxy_to(
    state: ProxyState,
    headers: HeaderMap,
    request: Request<Body>,
    path: String,
    admin: bool,
) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let query = parts
        .uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let base = if admin {
        &state.admin_base
    } else {
        &state.stream_base
    };
    let url = format!("{base}{path}{query}");
    let mut builder = state
        .client
        .request(parts.method, url)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()));
    for (name, value) in headers {
        if let Some(name) = name {
            if name != header::HOST {
                builder = builder.header(name, value);
            }
        }
    }

    match builder.send().await {
        Ok(response) => {
            let status = response.status();
            let mut response_builder = Response::builder().status(status);
            for (name, value) in response.headers() {
                if name != header::CONTENT_LENGTH {
                    response_builder = response_builder.header(name, value);
                }
            }
            response_builder
                .body(Body::from_stream(response.bytes_stream()))
                .unwrap_or_else(|err| {
                    text_response(
                        StatusCode::BAD_GATEWAY,
                        format!("proxy response error: {err}"),
                    )
                })
        }
        Err(err) => text_response(StatusCode::BAD_GATEWAY, format!("proxy error: {err}")),
    }
}

fn text_response(status: StatusCode, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn is_unsafe_admin_auth_value(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "" | "admin"
            | "administrator"
            | "changeme"
            | "change-me"
            | "change_me"
            | "default"
            | "password"
            | "please-change-me"
            | "replace-me"
            | "secret"
            | "test"
            | "test-secret"
    ) || [
        "change-me",
        "change_me",
        "changeme",
        "change-this",
        "change_this",
        "changethis",
        "replace-me",
        "replace_me",
        "replace-this",
        "replace_this",
    ]
    .iter()
    .any(|placeholder| normalized.contains(placeholder))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_rejects_default_credentials() {
        let setup = FirstRunSetup {
            admin_username: "admin".to_string(),
            admin_password: "admin".to_string(),
            wallet_key: Some("0xabc".to_string()),
            wallet_key_file: None,
        };
        assert!(validate_setup(&setup).is_err());
    }

    #[test]
    fn setup_accepts_strong_credentials_and_wallet_file() {
        let wallet_path =
            env::temp_dir().join(format!("autvid-wallet-key-test-{}", std::process::id()));
        fs::write(&wallet_path, "wallet-key").unwrap();
        let setup = FirstRunSetup {
            admin_username: "video-owner".to_string(),
            admin_password: "correct horse battery staple".to_string(),
            wallet_key: None,
            wallet_key_file: Some(wallet_path.to_string_lossy().to_string()),
        };
        assert!(validate_setup(&setup).is_ok());
        let _ = fs::remove_file(wallet_path);
    }

    #[test]
    fn setup_rejects_missing_wallet_file() {
        let setup = FirstRunSetup {
            admin_username: "video-owner".to_string(),
            admin_password: "correct horse battery staple".to_string(),
            wallet_key: None,
            wallet_key_file: Some("/tmp/autvid-missing-wallet-key".to_string()),
        };
        assert!(validate_setup(&setup).is_err());
    }

    #[test]
    fn env_port_uses_env_value() {
        env::set_var("AUTVID_TEST_PORT", "45678");
        assert_eq!(env_port_or_available("AUTVID_TEST_PORT", 1).unwrap(), 45678);
        env::remove_var("AUTVID_TEST_PORT");
    }

    #[test]
    fn desktop_admin_auth_env_is_applied_to_admin_command() {
        let mut command = Command::new("rust_admin");
        apply_desktop_admin_auth_env(&mut command);

        for (expected_key, expected_value) in desktop_admin_auth_env() {
            let value = command
                .as_std()
                .get_envs()
                .find(|(key, _)| *key == std::ffi::OsStr::new(expected_key))
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned());
            assert_eq!(value.as_deref(), Some(*expected_value));
        }
    }

    #[test]
    fn stream_proxy_preserves_stream_prefix_for_sidecar_routes() {
        assert_eq!(
            stream_proxy_path("manifest/address/720p/playlist.m3u8"),
            "/stream/manifest/address/720p/playlist.m3u8"
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_name_matches_target_triple_sidecar() {
        assert!(process_name_is_expected("rust_admin", "rust_admin"));
        assert!(process_name_is_expected(
            "rust_admin-aarch64-apple-darwin",
            "rust_admin"
        ));
        assert!(!process_name_is_expected("rust_admin_backup", "rust_admin"));
    }
}
