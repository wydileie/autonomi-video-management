use std::{
    env,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{anyhow, Context};
use axum::{
    routing::{any, get},
    Router,
};
use reqwest::Client;
use tokio::{
    net::TcpListener,
    process::{Child, Command},
    task::JoinHandle,
    time::{sleep, Instant},
};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};

use super::*;

#[derive(Debug)]
pub struct RunningStack {
    pub url: String,
    _lock_file: File,
    children: Vec<ManagedChild>,
    server: JoinHandle<anyhow::Result<()>>,
}

#[derive(Debug)]
pub(crate) struct ManagedChild {
    pub(crate) name: String,
    pub(crate) child: Child,
    pub(crate) pid_file: PathBuf,
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

pub(crate) fn start_antd(
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
pub(crate) fn start_rust_admin(
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

pub(crate) fn desktop_admin_auth_env() -> &'static [(&'static str, &'static str)] {
    &[
        ("APP_ENV", "production"),
        ("ADMIN_AUTH_COOKIE_SECURE", "false"),
        ("AUTVID_STRICT_AUTH", "true"),
    ]
}

pub(crate) fn apply_desktop_admin_auth_env(command: &mut Command) {
    for (key, value) in desktop_admin_auth_env() {
        command.env(key, value);
    }
}

pub(crate) fn start_rust_stream(
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

pub(crate) fn child_command(env_name: &str, fallback: &str, binary_dir: Option<&Path>) -> Command {
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

pub(crate) fn spawn(
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

pub(crate) fn append_log(logs_dir: &Path, file_name: &str) -> anyhow::Result<std::fs::File> {
    fs::create_dir_all(logs_dir)?;
    Ok(OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs_dir.join(file_name))?)
}

pub(crate) async fn wait_for_health(url: &str, name: &str) -> anyhow::Result<()> {
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

pub(crate) async fn stop_children(children: &mut [ManagedChild]) {
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
