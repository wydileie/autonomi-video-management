use std::{
    env,
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
use reqwest::Client;
use tokio::{
    process::{Child, Command},
    time::sleep,
};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Clone, Copy, Eq, PartialEq)]
enum NetworkMode {
    Configured,
    LocalDevnet,
}

struct Options {
    mode: NetworkMode,
    no_open: bool,
}

#[derive(Clone)]
struct ProxyState {
    admin_base: String,
    client: Client,
    runtime_config_js: String,
    stream_base: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let options = parse_options()?;
    let data_dir = resolve_data_dir()?;
    let processing_dir = data_dir.join("processing");
    let catalog_dir = data_dir.join("catalog");
    tokio::fs::create_dir_all(&processing_dir).await?;
    tokio::fs::create_dir_all(&catalog_dir).await?;

    let admin_port = env_u16("RUST_ADMIN_PORT", 8000);
    let stream_port = env_u16("RUST_STREAM_PORT", 8081);
    let antd_port = env_u16("ANTD_REST_PORT", 8082);
    let launcher_port = env_u16("AUTVID_LAUNCHER_PORT", 8080);

    let antd_url = format!("http://127.0.0.1:{antd_port}");
    let admin_base = format!("http://127.0.0.1:{admin_port}");
    let stream_base = format!("http://127.0.0.1:{stream_port}");
    let launcher_url = format!("http://127.0.0.1:{launcher_port}");

    let runtime_config_js =
        "window.__AUTONOMI_VIDEO_CONFIG__ = { apiBaseUrl: '/api', streamBaseUrl: '/stream' };\n"
            .to_string();
    let runtime_config_path = data_dir.join("frontend-runtime").join("runtime-config.js");
    if let Some(parent) = runtime_config_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&runtime_config_path, &runtime_config_js).await?;

    let mut children = Vec::new();
    children.push(start_antd(options.mode, &data_dir, antd_port).await?);
    wait_for_health(&format!("{antd_url}/livez"), "antd").await?;

    children.push(
        start_rust_admin(
            &data_dir,
            &processing_dir,
            &catalog_dir,
            admin_port,
            &antd_url,
            &launcher_url,
        )
        .await?,
    );
    children.push(start_rust_stream(stream_port, &antd_url, &catalog_dir, &launcher_url).await?);

    wait_for_health(&format!("{admin_base}/livez"), "rust_admin").await?;
    wait_for_health(&format!("{stream_base}/livez"), "rust_stream").await?;

    let frontend_dir = resolve_frontend_dir()?;
    let state = ProxyState {
        admin_base,
        client: Client::new(),
        runtime_config_js,
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

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", launcher_port)).await?;
    info!("AutVid launcher ready at {}", launcher_url);
    if !options.no_open {
        open_browser(&launcher_url);
    }

    let server = axum::serve(listener, app);
    tokio::select! {
        result = server => result?,
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown requested");
        }
    }

    stop_children(&mut children).await;
    Ok(())
}

fn parse_options() -> anyhow::Result<Options> {
    let mut mode = NetworkMode::Configured;
    let mut no_open = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--configured-network" => mode = NetworkMode::Configured,
            "--local-devnet" => mode = NetworkMode::LocalDevnet,
            "--mode" => {
                let value = args.next().ok_or_else(|| anyhow!("--mode needs a value"))?;
                mode = match value.as_str() {
                    "configured" | "configured-network" => NetworkMode::Configured,
                    "local" | "local-devnet" | "devnet" => NetworkMode::LocalDevnet,
                    _ => return Err(anyhow!("unknown launcher mode: {value}")),
                };
            }
            "--no-open" => no_open = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => return Err(anyhow!("unknown argument: {arg}")),
        }
    }
    Ok(Options { mode, no_open })
}

fn print_help() {
    println!("Usage: autvid_launcher [--mode configured|local-devnet] [--no-open]");
}

fn resolve_data_dir() -> anyhow::Result<PathBuf> {
    if let Ok(value) = env::var("AUTVID_DATA_DIR") {
        return Ok(PathBuf::from(value));
    }
    let home = env::var("HOME").context("HOME is required when AUTVID_DATA_DIR is unset")?;
    if cfg!(target_os = "macos") {
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Autonomi Video Management"))
    } else {
        Ok(env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(home).join(".local").join("share"))
            .join("autonomi-video-management"))
    }
}

fn resolve_frontend_dir() -> anyhow::Result<PathBuf> {
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
    let bundled = exe_dir.join("frontend");
    if bundled.join("index.html").is_file() {
        return Ok(bundled);
    }
    Err(anyhow!(
        "frontend build not found; set AUTVID_FRONTEND_DIR to a directory containing index.html"
    ))
}

async fn start_antd(mode: NetworkMode, data_dir: &Path, antd_port: u16) -> anyhow::Result<Child> {
    match mode {
        NetworkMode::Configured => {
            let mut command = child_command("AUTVID_ANTD_BIN", "antd");
            command
                .env("ANTD_REST_ADDR", format!("127.0.0.1:{antd_port}"))
                .env("ANTD_UPLOAD_TEMP_DIR", data_dir.join("antd-temp"));
            spawn("antd", command)
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
            spawn("local-devnet", command)
        }
    }
}

async fn start_rust_admin(
    data_dir: &Path,
    processing_dir: &Path,
    catalog_dir: &Path,
    admin_port: u16,
    antd_url: &str,
    launcher_url: &str,
) -> anyhow::Result<Child> {
    let mut command = child_command("AUTVID_ADMIN_BIN", "rust_admin");
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
        .env("CORS_ALLOWED_ORIGINS", launcher_url)
        .env(
            "ADMIN_DB_MIN_CONNECTIONS",
            env::var("ADMIN_DB_MIN_CONNECTIONS").unwrap_or_else(|_| "1".into()),
        )
        .env(
            "ADMIN_DB_MAX_CONNECTIONS",
            env::var("ADMIN_DB_MAX_CONNECTIONS").unwrap_or_else(|_| "5".into()),
        );
    spawn("rust_admin", command)
}

async fn start_rust_stream(
    stream_port: u16,
    antd_url: &str,
    catalog_dir: &Path,
    launcher_url: &str,
) -> anyhow::Result<Child> {
    let mut command = child_command("AUTVID_STREAM_BIN", "rust_stream");
    command
        .env("ANTD_URL", antd_url)
        .env("CATALOG_STATE_PATH", catalog_dir.join("catalog.json"))
        .env("RUST_STREAM_PORT", stream_port.to_string())
        .env("CORS_ALLOWED_ORIGINS", launcher_url);
    spawn("rust_stream", command)
}

fn child_command(env_name: &str, fallback: &str) -> Command {
    if let Ok(path) = env::var(env_name) {
        return Command::new(path);
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

fn spawn(name: &str, mut command: Command) -> anyhow::Result<Child> {
    info!("Starting {}", name);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("could not start {name}"))
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

fn env_u16(name: &str, default: u16) -> u16 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(err) = Command::new(opener).arg(url).spawn() {
        warn!("Could not open browser with {}: {}", opener, err);
    }
}

async fn stop_children(children: &mut [Child]) {
    for child in children.iter_mut() {
        if let Err(err) = child.start_kill() {
            warn!("Could not signal child shutdown: {}", err);
        }
    }
    for child in children.iter_mut() {
        let _ = child.wait().await;
    }
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
    proxy_to(state, headers, request, format!("/{}", path), false).await
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
            match response.bytes().await {
                Ok(bytes) => response_builder
                    .body(Body::from(bytes))
                    .unwrap_or_else(|err| {
                        text_response(
                            StatusCode::BAD_GATEWAY,
                            format!("proxy response error: {err}"),
                        )
                    }),
                Err(err) => {
                    text_response(StatusCode::BAD_GATEWAY, format!("proxy read error: {err}"))
                }
            }
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
